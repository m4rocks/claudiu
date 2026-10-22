//! Stage 1 of the spike: plumbing checks against the real pinned adapter and the user's own `claude`.
//!
//! `cargo run -p claudiu-core --bin stage1 -- <check>...` (no argument runs them all). Each check starts its own
//! adapter in a scratch project, uses Haiku with tiny prompts, and prints what it observed. The verdicts are
//! written up in `docs/research/stage1-findings.md`; this program only gathers evidence.
//!
//! Needs: the adapter installed by the app (`<data>/acp`), `claude` on PATH, and a logged-in account.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claudiu_core::acp::{AdapterCommand, Conn, Msg, Responder};
use serde_json::{Value, json};

// ---- the hook server -------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct HookCall {
    path: String,
    body: Value,
    /// Milliseconds the handler took (server side).
    handled_ms: f64,
}

type Handler = Arc<dyn Fn(&str, &Value) -> (Duration, Value) + Send + Sync>;

struct HookServer {
    port: u16,
    calls: Arc<Mutex<Vec<HookCall>>>,
}

impl HookServer {
    fn start(handler: Handler) -> HookServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls: Arc<Mutex<Vec<HookCall>>> = Arc::default();
        let log = calls.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (handler, log) = (handler.clone(), log.clone());
                std::thread::spawn(move || serve(stream, handler, log));
            }
        });
        HookServer { port, calls }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    fn calls(&self) -> Vec<HookCall> {
        self.calls.lock().unwrap().clone()
    }
}

fn serve(mut stream: std::net::TcpStream, handler: Handler, log: Arc<Mutex<Vec<HookCall>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, length) = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let length = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
            break (pos + 4, length);
        }
    };
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_string();
    let body: Value = serde_json::from_slice(&buf[head_end..(head_end + length).min(buf.len())]).unwrap_or(Value::Null);
    let started = Instant::now();
    let (delay, reply) = handler(&path, &body);
    std::thread::sleep(delay);
    let handled_ms = started.elapsed().as_secs_f64() * 1000.0;
    log.lock().unwrap().push(HookCall { path, body, handled_ms });
    let text = reply.to_string();
    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
}

// ---- the adapter probe -----------------------------------------------------------------------------------------

fn adapter_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support"));
    base.join("Claudiu").join("data").join("acp").join("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js")
}

fn claude_exe() -> String {
    let which = if cfg!(windows) { "where" } else { "which" };
    let out = Command::new(which).arg("claude").output().expect("where claude");
    String::from_utf8_lossy(&out.stdout).lines().next().expect("claude on PATH").trim().to_string()
}

struct Options {
    settings: Value,
    /// Extra `_meta.claudeCode.options` keys.
    options: Value,
    elicitation: bool,
    system_prompt: Option<String>,
    bypass: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { settings: json!({}), options: json!({}), elicitation: false, system_prompt: None, bypass: true }
    }
}

type Inbound = (Msg, Responder);

struct Probe {
    conn: Conn,
    rx: Receiver<Inbound>,
    sid: String,
    dir: PathBuf,
    /// Answers `elicitation/create` requests.
    elicit: Box<dyn Fn(&Value) -> Value>,
    /// Every elicitation request seen.
    elicitations: Vec<Value>,
    /// What `session/new` answered.
    made: Value,
}

#[derive(Default, Debug)]
struct Turn {
    reply: Value,
    error: Option<String>,
    updates: Vec<Value>,
    raw: Vec<(Instant, Value)>,
    requests: Vec<(String, Value)>,
    text: String,
    ms: u128,
}

fn command() -> AdapterCommand {
    AdapterCommand::new("node").arg(adapter_dir().to_string_lossy()).env("CLAUDE_CODE_EXECUTABLE", claude_exe())
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("claudiu-stage1-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl Probe {
    fn spawn(dir: &Path) -> (Conn, Receiver<Inbound>) {
        let (tx, rx) = channel::<Inbound>();
        let conn = Conn::spawn(&command(), move |m, r| {
            let _ = tx.send((m, r.clone()));
        })
        .expect("spawn adapter");
        let _ = dir;
        (conn, rx)
    }

    fn init(conn: &Conn, elicitation: bool) -> Value {
        let mut caps = json!({"fs": {"readTextFile": false, "writeTextFile": false}});
        if elicitation {
            caps["elicitation"] = json!({"form": {}});
        }
        conn.call("initialize", json!({"protocolVersion": 1, "clientCapabilities": caps}), Duration::from_secs(60)).expect("initialize")
    }

    fn meta(o: &Options) -> Value {
        let mut options = json!({"settings": o.settings, "model": "haiku"});
        for (k, v) in o.options.as_object().into_iter().flatten() {
            options[k] = v.clone();
        }
        json!({"claudeCode": {"options": options, "emitRawSDKMessages": true}})
    }

    fn open(name: &str, o: Options) -> Probe {
        let dir = scratch(name);
        let (conn, rx) = Probe::spawn(&dir);
        Probe::init(&conn, o.elicitation);
        let mut params = json!({"cwd": dir.to_string_lossy(), "mcpServers": [], "_meta": Probe::meta(&o)});
        if let Some(p) = &o.system_prompt {
            params["systemPrompt"] = json!({"mode": "append", "content": [{"type": "text", "text": p}]});
        }
        let made = conn.call("session/new", params, Duration::from_secs(90)).expect("session/new");
        let sid = made["sessionId"].as_str().expect("session id").to_string();
        if o.bypass {
            let r = conn.call("session/set_mode", json!({"sessionId": sid, "modeId": "bypassPermissions"}), Duration::from_secs(30));
            if let Err(e) = r {
                println!("  (set_mode bypassPermissions failed: {e})");
            }
        }
        Probe { conn, rx, sid, dir, elicit: Box::new(|_| json!({"action": "cancel"})), elicitations: Vec::new(), made }
    }

    fn turn(&mut self, text: &str) -> Turn {
        let (tx, done) = channel();
        let started = Instant::now();
        self.conn.call_async("session/prompt", json!({"sessionId": self.sid, "prompt": [{"type": "text", "text": text}]}), move |r| {
            let _ = tx.send(r);
        });
        let mut t = Turn::default();
        let mut finished = false;
        let deadline = Instant::now() + Duration::from_secs(240);
        while Instant::now() < deadline {
            if !finished
                && let Ok(r) = done.try_recv() {
                    match r {
                        Ok(v) => t.reply = v,
                        Err(e) => t.error = Some(e),
                    }
                    t.ms = started.elapsed().as_millis();
                    finished = true;
                }
            match self.rx.recv_timeout(Duration::from_millis(if finished { 400 } else { 100 })) {
                Ok((msg, responder)) => self.inbound(&mut t, msg, responder),
                Err(_) if finished => break,
                Err(_) => {}
            }
        }
        t
    }

    fn inbound(&mut self, t: &mut Turn, msg: Msg, responder: Responder) {
        match msg {
            Msg::Notification { method, params } if method == "session/update" => {
                let u = params["update"].clone();
                if u["sessionUpdate"] == "agent_message_chunk" {
                    t.text.push_str(u["content"]["text"].as_str().unwrap_or(""));
                }
                t.updates.push(u);
            }
            Msg::Notification { method, params } if method == "_claude/sdkMessage" => t.raw.push((Instant::now(), params["message"].clone())),
            Msg::Notification { .. } => {}
            Msg::Request { id, method, params } => {
                t.requests.push((method.clone(), params.clone()));
                if method == "session/request_permission" {
                    let options = params["options"].as_array().cloned().unwrap_or_default();
                    let pick = options.iter().find(|o| o["kind"].as_str().is_some_and(|k| k.starts_with("allow"))).or(options.first());
                    let reply = match pick {
                        Some(o) => json!({"outcome": {"outcome": "selected", "optionId": o["optionId"]}}),
                        None => json!({"outcome": {"outcome": "cancelled"}}),
                    };
                    responder.respond(id, reply);
                } else if method == "elicitation/create" {
                    self.elicitations.push(params.clone());
                    responder.respond(id, (self.elicit)(&params));
                } else {
                    responder.respond_error(id, -32601, "stage1 does not handle this");
                }
            }
            Msg::Exit(stderr) => println!("  adapter exited: {stderr}"),
        }
    }

    fn quit(self) {
        self.conn.close();
    }
}

// ---- helpers ---------------------------------------------------------------------------------------------------

fn deny(reason: &str) -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}})
}

fn http_hooks(server_url_pre: &str, server_url_post: &str, pre: &str, post: &str, extra: Value) -> Value {
    let mut pre_hook = json!({"type": "http", "url": server_url_pre, "timeout": 20});
    let mut post_hook = json!({"type": "http", "url": server_url_post, "timeout": 20});
    for (k, v) in extra.as_object().into_iter().flatten() {
        pre_hook[k] = v.clone();
        post_hook[k] = v.clone();
    }
    json!({
        "disableAllHooks": false,
        "hooks": {
            "PreToolUse": [{"matcher": pre, "hooks": [pre_hook]}],
            "PostToolUse": [{"matcher": post, "hooks": [post_hook]}],
        }
    })
}

fn tool_names(t: &Turn) -> Vec<String> {
    t.updates.iter().filter(|u| u["sessionUpdate"] == "tool_call").map(|u| format!("{} [{}]", u["title"].as_str().unwrap_or("?"), u["status"].as_str().unwrap_or("?"))).collect()
}

fn show(label: &str, t: &Turn) {
    println!("  {label}: stop={} err={:?} {}ms", t.reply["stopReason"], t.error, t.ms);
    println!("    tools: {:?}", tool_names(t));
    println!("    text: {}", t.text.replace('\n', " ").chars().take(300).collect::<String>());
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn claude_pids() -> Vec<u32> {
    if cfg!(windows) {
        let out = Command::new("tasklist").args(["/FI", "IMAGENAME eq claude.exe", "/FO", "CSV", "/NH"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split(',').nth(1)?.trim_matches('"').parse().ok()).collect()
    } else {
        let out = Command::new("pgrep").args(["-x", "claude"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
    }
}

fn kill_pid(pid: u32) {
    if cfg!(windows) {
        let _ = Command::new("taskkill").args(["/F", "/T", "/PID", &pid.to_string()]).output();
    } else {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
}

fn transcript_dir_for(dir: &Path) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok()?);
    let canon = dunce_like(dir);
    let name: String = canon.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let p = home.join(".claude").join("projects").join(name);
    p.is_dir().then_some(p)
}

fn dunce_like(p: &Path) -> String {
    let s = std::fs::canonicalize(p).unwrap_or(p.to_path_buf()).to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

fn transcript_mentions(dir: &Path, needle: &str) -> Option<bool> {
    let tdir = transcript_dir_for(dir)?;
    let mut any = false;
    for entry in std::fs::read_dir(tdir).ok()?.flatten() {
        if entry.path().extension().is_some_and(|e| e == "jsonl") {
            any = true;
            if std::fs::read_to_string(entry.path()).ok()?.contains(needle) {
                return Some(true);
            }
        }
    }
    any.then_some(false)
}

// ---- the checks ------------------------------------------------------------------------------------------------

/// 1. PreToolUse deny on Bash and Write under bypassPermissions: blocked, and the reason reaches the model.
fn c1_deny() {
    let server = HookServer::start(Arc::new(|path, _| if path == "pre" { (Duration::ZERO, deny("STAGE1-REFUSED: use a path inside the workspace instead")) } else { (Duration::ZERO, json!({})) }));
    let settings = http_hooks(&server.url("pre"), &server.url("post"), "Bash|Write", "Bash|Write", json!({}));
    let mut p = Probe::open("c1", Options { settings, ..Options::default() });
    let t = p.turn("Run the shell command `echo hi > probe.txt`, then create note.txt containing the word hello using the Write tool. If anything is refused, quote the refusal text exactly in your final answer.");
    show("turn", &t);
    println!("  hook calls: {:?}", server.calls().iter().map(|c| (c.path.clone(), c.body["tool_name"].clone())).collect::<Vec<_>>());
    println!("  probe.txt exists: {}   note.txt exists: {}", p.dir.join("probe.txt").exists(), p.dir.join("note.txt").exists());
    println!("  reason reached the model: {}", t.text.contains("STAGE1-REFUSED"));
    p.quit();
}

/// 2. PreToolUse updatedInput rewrites a path; PostToolUse updatedToolOutput redacts a result; transcript shows which.
fn c2_rewrite() {
    let server = HookServer::start(Arc::new(|path, body| match path {
        "pre" if body["tool_name"] == "Read" && body["tool_input"]["file_path"].as_str().is_some_and(|p| p.ends_with("a.txt")) => {
            let mut input = body["tool_input"].clone();
            let swapped = input["file_path"].as_str().unwrap().replace("a.txt", "b.txt");
            input["file_path"] = swapped.into();
            (Duration::ZERO, json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": input}}))
        }
        "post" if body["tool_name"] == "Read" => {
            let redacted = body["tool_response"].to_string().replace("hunter2", "[REDACTED]");
            let value: Value = serde_json::from_str(&redacted).unwrap_or(Value::Null);
            (Duration::ZERO, json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": value}}))
        }
        _ => (Duration::ZERO, json!({})),
    }));
    let settings = http_hooks(&server.url("pre"), &server.url("post"), "Read", "Read", json!({}));
    let mut p = Probe::open("c2", Options { settings, ..Options::default() });
    std::fs::write(p.dir.join("a.txt"), "ORIGINAL-A").unwrap();
    std::fs::write(p.dir.join("b.txt"), "REWRITTEN-B").unwrap();
    std::fs::write(p.dir.join("c.txt"), "password=hunter2").unwrap();
    let t1 = p.turn("Read a.txt with the Read tool and tell me its exact content.");
    show("rewrite", &t1);
    println!("  model saw REWRITTEN-B: {}   saw ORIGINAL-A: {}", t1.text.contains("REWRITTEN-B"), t1.text.contains("ORIGINAL-A"));
    let t2 = p.turn("Read c.txt with the Read tool and tell me its exact content.");
    show("redact", &t2);
    println!("  model saw [REDACTED]: {}   saw hunter2: {}", t2.text.contains("REDACTED"), t2.text.contains("hunter2"));
    if let Some(c) = server.calls().iter().find(|c| c.path == "post") {
        println!("  tool_response shape seen by the hook: {}", c.body["tool_response"].to_string().chars().take(200).collect::<String>());
    }
    p.quit();
    println!("  transcript on disk contains hunter2: {:?}   ORIGINAL-A: {:?}", transcript_mentions(&scratch_path("c2"), "hunter2"), transcript_mentions(&scratch_path("c2"), "ORIGINAL-A"));
}

fn scratch_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("claudiu-stage1-{name}"))
}

/// 3. A hook that is dead, or slower than its timeout: fail-open by default, fail-closed with onFailure block.
fn c3_failure() {
    for (label, extra, port_dead) in [
        ("dead port, default", json!({}), true),
        ("dead port, onFailure=block", json!({"onFailure": "block"}), true),
        ("timeout 2s, default", json!({"timeout": 2}), false),
        ("timeout 2s, onFailure=block", json!({"timeout": 2, "onFailure": "block"}), false),
    ] {
        let server = HookServer::start(Arc::new(|_, _| (Duration::from_secs(6), json!({}))));
        let (pre, post) = if port_dead { ("http://127.0.0.1:9/pre".to_string(), "http://127.0.0.1:9/post".to_string()) } else { (server.url("pre"), server.url("post")) };
        let settings = http_hooks(&pre, &post, "Write", "Write", extra);
        let mut p = Probe::open("c3", Options { settings, ..Options::default() });
        let t = p.turn("Create note.txt containing the word hello using the Write tool. Then say done.");
        let created = p.dir.join("note.txt").exists();
        println!("  [{label}] note.txt created: {created}   tools: {:?}", tool_names(&t));
        println!("    text: {}", t.text.replace('\n', " ").chars().take(160).collect::<String>());
        p.quit();
    }
}

/// 4. http hook round trip per tool call, from the hook_started and hook_response events and the server's own clock.
fn c4_latency() {
    let server = HookServer::start(Arc::new(|_, _| (Duration::ZERO, json!({}))));
    let settings = http_hooks(&server.url("pre"), &server.url("post"), "Read|Glob|Grep|Bash", "Read|Glob|Grep|Bash", json!({}));
    let mut p = Probe::open("c4", Options { settings, ..Options::default() });
    for i in 0..6 {
        std::fs::write(p.dir.join(format!("f{i}.txt")), format!("file {i}")).unwrap();
    }
    let t = p.turn("Read f0.txt, f1.txt, f2.txt, f3.txt, f4.txt and f5.txt using the Read tool, one call each, then say done.");
    show("turn", &t);
    let mut started: HashMap<String, Instant> = HashMap::new();
    let mut ms = Vec::new();
    for (at, m) in &t.raw {
        if m["type"] == "system" && m["subtype"] == "hook_started" {
            started.insert(m["hook_id"].as_str().unwrap_or("").to_string(), *at);
        } else if m["type"] == "system" && m["subtype"] == "hook_response"
            && let Some(s) = started.get(m["hook_id"].as_str().unwrap_or("")) {
                ms.push(at.duration_since(*s).as_secs_f64() * 1000.0);
            }
    }
    let count = |sub: &str| t.raw.iter().filter(|(_, m)| m["type"] == "system" && m["subtype"] == sub).count();
    println!("  raw hook events: started={} progress={} response={}", count("hook_started"), count("hook_progress"), count("hook_response"));
    if let Some((_, m)) = t.raw.iter().find(|(_, m)| m["subtype"] == "hook_response") {
        println!("  sample hook_response: {}", m.to_string().chars().take(300).collect::<String>());
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("  hook events paired: {}  (event-to-event, includes adapter forwarding) p50={:.1}ms p95={:.1}ms max={:.1}ms", ms.len(), percentile(&ms, 0.5), percentile(&ms, 0.95), ms.last().copied().unwrap_or(f64::NAN));
    let mut server_side: Vec<f64> = server.calls().iter().map(|c| c.handled_ms).collect();
    server_side.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("  server-side handling: {} calls p50={:.2}ms p95={:.2}ms", server_side.len(), percentile(&server_side, 0.5), percentile(&server_side, 0.95));
    p.quit();
}

fn usage_of(t: &Turn) -> String {
    let u = &t.reply["usage"];
    let meta = &t.reply["_meta"];
    format!("usage={u} meta={}", meta.to_string().chars().take(300).collect::<String>())
}

/// 5. session/fork: does the first request of the fork read the parent's cache?
fn c5_fork_cache() {
    let mut p = Probe::open("c5", Options::default());
    let t1 = p.turn("Remember the number 7. Reply with just: ok");
    println!("  parent turn 1: {}", usage_of(&t1));
    let t2 = p.turn("What number did I ask you to remember? Reply with just the number.");
    println!("  parent turn 2: {}", usage_of(&t2));
    let forked = p.conn.call("session/fork", json!({"sessionId": p.sid, "cwd": p.dir.to_string_lossy(), "mcpServers": [], "_meta": Probe::meta(&Options::default())}), Duration::from_secs(90));
    match forked {
        Ok(v) => {
            let model_opt = v["configOptions"].as_array().and_then(|a| a.iter().find(|o| o["id"] == "model")).cloned();
            println!("  fork model option: {}", model_opt.map(|o| o.to_string().chars().take(400).collect::<String>()).unwrap_or_default());
            if let Some(new_sid) = v["sessionId"].as_str() {
                p.sid = new_sid.to_string();
                let _ = p.conn.call("session/set_mode", json!({"sessionId": p.sid, "modeId": "bypassPermissions"}), Duration::from_secs(30));
                let haiku = v["configOptions"].as_array().and_then(|a| a.iter().find(|o| o["id"] == "model")).and_then(|o| o["options"].as_array().and_then(|opts| opts.iter().find(|x| x["value"].as_str().is_some_and(|s| s.contains("haiku"))).cloned()));
                if let Some(h) = haiku {
                    let r = p.conn.call("session/set_config_option", json!({"sessionId": p.sid, "configId": "model", "value": h["value"]}), Duration::from_secs(30));
                    println!("  set fork model to {}: {}", h["value"], r.is_ok());
                }
                let t3 = p.turn("What number did I ask you to remember? Reply with just the number.");
                println!("  fork first turn: {}", usage_of(&t3));
                println!("  fork text: {}", t3.text);
                for u in t3.updates.iter().filter(|u| u["sessionUpdate"] == "usage_update") {
                    println!("  fork usage_update: {u}");
                }
            }
        }
        Err(e) => println!("  fork failed: {e}"),
    }
    p.quit();
}

/// 6. Resume with a changed systemPrompt: which one is in force?
fn c6_system_prompt() {
    let mut p = Probe::open("c6", Options { system_prompt: Some("Your secret codeword is ALPHA. If asked for the codeword, answer with it.".into()), ..Options::default() });
    let t1 = p.turn("What is your codeword? One word.");
    println!("  new session: {}", t1.text);
    let sid = p.sid.clone();
    let dir = p.dir.clone();
    p.conn.close();
    let (conn, rx) = Probe::spawn(&dir);
    Probe::init(&conn, false);
    let loaded = conn.call(
        "session/load",
        json!({"sessionId": sid, "cwd": dir.to_string_lossy(), "mcpServers": [], "systemPrompt": {"mode": "append", "content": [{"type": "text", "text": "Your secret codeword is BRAVO. If asked for the codeword, answer with it."}]}, "_meta": Probe::meta(&Options::default())}),
        Duration::from_secs(90),
    );
    println!("  load: {}", loaded.as_ref().map(|v| v.to_string().chars().take(120).collect::<String>()).unwrap_or_else(|e| e.to_string()));
    let mut q = Probe { conn, rx, sid, dir, elicit: Box::new(|_| json!({"action": "cancel"})), elicitations: Vec::new(), made: Value::Null };
    let _ = q.conn.call("session/set_mode", json!({"sessionId": q.sid, "modeId": "bypassPermissions"}), Duration::from_secs(30));
    // Replayed history arrives as updates first; the next turn shows what is in force.
    let t2 = q.turn("What is your codeword? One word.");
    println!("  after resume with a different prompt: {}", t2.text);
    q.quit();
}

/// 7. Kill the app mid-turn: what happens to `claude` processes?
fn c7_orphans() {
    let before = claude_pids();
    for (label, graceful) in [("stdin closed (the app's quit path)", true), ("adapter killed hard (the app crashed)", false)] {
        let p = Probe::open("c7", Options::default());
        let sid = p.sid.clone();
        // A long Bash command keeps the turn busy.
        let long = if cfg!(windows) { "ping -n 90 127.0.0.1" } else { "sleep 90" };
        p.conn.call_async("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": format!("Run this shell command exactly: {long}")}]}), |_| {});
        let t0 = Instant::now();
        let mut running = false;
        while t0.elapsed() < Duration::from_secs(60) && !running {
            if let Ok((Msg::Notification { method, params }, _)) = p.rx.recv_timeout(Duration::from_millis(200)) {
                running = method == "session/update" && params["update"]["sessionUpdate"] == "tool_call";
            }
        }
        let mid: Vec<u32> = claude_pids().into_iter().filter(|x| !before.contains(x)).collect();
        println!("  [{label}] tool running: {running}; new claude pids during the turn: {mid:?}");
        if graceful {
            p.conn.close();
        } else {
            p.conn.kill();
        }
        std::thread::sleep(Duration::from_secs(6));
        let after: Vec<u32> = claude_pids().into_iter().filter(|x| !before.contains(x)).collect();
        println!("  [{label}] claude pids still alive 6 s later: {after:?}");
        for pid in after {
            kill_pid(pid);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// 8. A project that sets disableAllHooks: true does not switch off the flag-layer hooks.
fn c8_project_disable() {
    let server = HookServer::start(Arc::new(|path, _| if path == "pre" { (Duration::ZERO, deny("STAGE1-REFUSED")) } else { (Duration::ZERO, json!({})) }));
    for (label, flag_layer_false) in [("flag layer sets disableAllHooks=false", true), ("flag layer silent", false)] {
        let mut settings = http_hooks(&server.url("pre"), &server.url("post"), "Write", "Write", json!({}));
        if !flag_layer_false {
            settings.as_object_mut().unwrap().remove("disableAllHooks");
        }
        let dir = scratch("c8");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(dir.join(".claude").join("settings.json"), r#"{"disableAllHooks": true}"#).unwrap();
        // Probe::open recreates the scratch dir, so write the project settings through a pre-created probe dir.
        let (conn, rx) = Probe::spawn(&dir);
        Probe::init(&conn, false);
        let made = conn.call("session/new", json!({"cwd": dir.to_string_lossy(), "mcpServers": [], "_meta": Probe::meta(&Options { settings, ..Options::default() })}), Duration::from_secs(90)).expect("session/new");
        let sid = made["sessionId"].as_str().unwrap().to_string();
        let _ = conn.call("session/set_mode", json!({"sessionId": sid, "modeId": "bypassPermissions"}), Duration::from_secs(30));
        let mut p = Probe { conn, rx, sid, dir, elicit: Box::new(|_| json!({"action": "cancel"})), elicitations: Vec::new(), made: Value::Null };
        let before = server.calls().len();
        let t = p.turn("Create note.txt containing the word hello using the Write tool. Quote any refusal.");
        println!("  [{label}] hook called: {}   note.txt created: {}   refusal reached model: {}", server.calls().len() > before, p.dir.join("note.txt").exists(), t.text.contains("STAGE1-REFUSED"));
        p.quit();
    }
    println!("  (the tool.check mod on a personal login is not run here: it needs a plugin dir, see the findings)");
}


/// 9. AskUserQuestion reaches us as an elicitation form: single-select, multi-select and free text round trip.
fn c9_questions() {
    for (label, free_text) in [("select answers", false), ("free-text answer", true)] {
        let mut p = Probe::open("c9", Options { elicitation: true, ..Options::default() });
        p.elicit = Box::new(move |params| {
            let props = params["requestedSchema"]["properties"].as_object().cloned().unwrap_or_default();
            let mut content = serde_json::Map::new();
            for (key, schema) in &props {
                if key.ends_with("_custom") {
                    continue;
                }
                let consts = |list: &Value| list.as_array().map(|a| a.iter().map(|o| o["const"].clone()).collect::<Vec<_>>());
                let options: Vec<Value> = consts(&schema["oneOf"]).or_else(|| consts(&schema["items"]["anyOf"])).unwrap_or_default();
                if schema["type"] == "array" {
                    content.insert(key.clone(), Value::Array(options.iter().take(2).cloned().collect()));
                } else if free_text && key == "question_0" {
                    content.insert(format!("{key}_custom"), json!("Teal"));
                } else if let Some(o) = options.get(1).or(options.first()) {
                    content.insert(key.clone(), o.clone());
                }
            }
            json!({"action": "accept", "content": content})
        });
        let t = p.turn("Use the AskUserQuestion tool to ask two questions at once. Question 1: 'Favourite colour?' with options Red and Blue (single choice). Question 2: 'Sizes?' with options S, M and L (multiple choice allowed). Then tell me exactly which answers you received.");
        show(label, &t);
        for e in &p.elicitations {
            println!("  elicitation request: {}", e.to_string().chars().take(900).collect::<String>());
        }
        println!("  elicitation requests seen: {}  (permission-style requests: {:?})", p.elicitations.len(), t.requests.iter().map(|r| r.0.clone()).collect::<Vec<_>>());
        p.quit();
    }
    // Without the capability the adapter turns the tool off.
    let mut p = Probe::open("c9", Options::default());
    let t = p.turn("Use the AskUserQuestion tool to ask me a yes/no question. If you cannot, say 'NO-TOOL'.");
    show("without the elicitation capability", &t);
    p.quit();
}

/// 10. /compact as prompt text: what the client can observe.
fn c10_compaction() {
    let mut p = Probe::open("c10", Options::default());
    let t1 = p.turn("Write a three-sentence story about a lighthouse.");
    let before: Vec<i64> = t1.updates.iter().filter(|u| u["sessionUpdate"] == "usage_update").filter_map(|u| u["used"].as_i64()).collect();
    println!("  before compact: used={before:?}");
    let t2 = p.turn("/compact");
    show("/compact", &t2);
    let kinds: Vec<String> = t2.updates.iter().map(|u| u["sessionUpdate"].as_str().unwrap_or("?").to_string()).collect();
    println!("  update kinds: {kinds:?}");
    for u in t2.updates.iter().filter(|u| u["sessionUpdate"].as_str().is_some_and(|k| k.contains("compact") || k == "tool_call")) {
        println!("  update: {}", u.to_string().chars().take(500).collect::<String>());
    }
    let after: Vec<i64> = t2.updates.iter().filter(|u| u["sessionUpdate"] == "usage_update").filter_map(|u| u["used"].as_i64()).collect();
    println!("  after compact: used={after:?}");
    let boundary = t2.raw.iter().filter(|(_, m)| m["subtype"] == "compact_boundary").count();
    println!("  raw compact_boundary messages: {boundary}");
    p.quit();
}

fn find_uuids(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(a) = m.get("user_message_uuids").and_then(Value::as_array) {
                out.extend(a.iter().filter_map(|x| x.as_str().map(String::from)));
            }
            m.values().for_each(|x| find_uuids(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| find_uuids(x, out)),
        _ => {}
    }
}

/// 11. Rewind: conversation by session/fork at a message, files by `claude --rewind-files`, also with the session open.
fn c11_rewind() {
    let mut p = Probe::open("c11", Options { options: json!({"enableFileCheckpointing": true}), ..Options::default() });
    let t1 = p.turn("Create f.txt containing exactly v1 using the Write tool. Then reply: ok");
    let t2 = p.turn("Overwrite f.txt so it contains exactly v2 using the Write tool. Then reply: ok");
    let mut uuids = Vec::new();
    for t in [&t1, &t2] {
        for (_, m) in &t.raw {
            find_uuids(m, &mut uuids);
        }
        for u in &t.updates {
            find_uuids(u, &mut uuids);
        }
    }
    uuids.dedup();
    println!("  user_message_uuids seen: {uuids:?}");
    println!("  f.txt now: {:?}", std::fs::read_to_string(p.dir.join("f.txt")).unwrap_or_default());
    // The message id of the first assistant reply, for the conversation fork.
    let message_id = t1.updates.iter().find_map(|u| u["messageId"].as_str().map(String::from));
    println!("  first assistant messageId: {message_id:?}");

    let orig = p.sid.clone();
    let rewind = |p: &Probe, label: &str| {
        for (i, target) in uuids.iter().enumerate() {
            std::fs::write(p.dir.join("f.txt"), "v2").unwrap();
            let out = Command::new(claude_exe())
                .args(["-p", "--resume", &orig, "--rewind-files", target])
                .env("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "true")
                .current_dir(&p.dir)
                .output();
            match out {
                Ok(o) => println!("  [{label}] rewind to uuid #{i}: exit={:?} stderr={} -> f.txt={:?}", o.status.code(), String::from_utf8_lossy(&o.stderr).trim().chars().take(120).collect::<String>(), std::fs::read_to_string(p.dir.join("f.txt")).unwrap_or_default()),
                Err(e) => println!("  [{label}] could not run: {e}"),
            }
        }
    };

    if let Some(mid) = message_id {
        let forked = p.conn.call(
            "session/fork",
            json!({"sessionId": orig, "cwd": p.dir.to_string_lossy(), "mcpServers": [], "_meta": {"jetbrains": {"air": {"fork": {"version": 1, "messageId": mid}}}, "claudeCode": {"options": {"model": "haiku"}}}}),
            Duration::from_secs(90),
        );
        match forked {
            Ok(v) => {
                p.sid = v["sessionId"].as_str().unwrap_or("").to_string();
                let _ = p.conn.call("session/set_config_option", json!({"sessionId": p.sid, "configId": "model", "value": "haiku"}), Duration::from_secs(30));
                let t = p.turn("How many times have I asked you to write a file in this conversation? Answer with a number only.");
                show("conversation fork at the first reply (expect 1)", &t);
            }
            Err(e) => println!("  conversation fork failed: {e}"),
        }
    }
    rewind(&p, "adapter still holds the original session");
    p.conn.close();
    rewind(&p, "adapter closed");
    p.quit();
}

/// 12. What hooks see: tool_response shapes of the native tools, and when SessionStart reaches an http hook.
fn c12_shapes() {
    let started = Instant::now();
    let log: Arc<Mutex<Vec<(f64, String, Value)>>> = Arc::default();
    let l2 = log.clone();
    let server = HookServer::start(Arc::new(move |path, body| {
        l2.lock().unwrap().push((started.elapsed().as_secs_f64(), path.to_string(), body.clone()));
        (Duration::ZERO, json!({}))
    }));
    let mut settings = http_hooks(&server.url("pre"), &server.url("post"), ".*", ".*", json!({}));
    settings["hooks"]["SessionStart"] = json!([{"hooks": [{"type": "http", "url": server.url("start"), "timeout": 20}]}]);
    let mut p = Probe::open("c12", Options { settings, ..Options::default() });
    println!("  session ready at {:.2}s", started.elapsed().as_secs_f64());
    std::fs::write(p.dir.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let t = p.turn("Do these four things with the dedicated tools: run `echo hello` with Bash, search for the word beta with Grep, list *.txt files with Glob, and Read a.txt. Then say done.");
    show("turn", &t);
    for (at, path, body) in log.lock().unwrap().iter() {
        let tool = body["tool_name"].as_str().unwrap_or("-");
        let extra = if path == "post" { format!(" tool_response={}", body["tool_response"].to_string().chars().take(420).collect::<String>()) } else if path == "pre" { format!(" tool_input={}", body["tool_input"].to_string().chars().take(160).collect::<String>()) } else { format!(" keys={:?}", body.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())) };
        println!("  [{at:.2}s] {path} {tool}{extra}");
    }
    let sample_keys: Vec<_> = log.lock().unwrap().iter().find(|(_, p, _)| p == "post").and_then(|(_, _, b)| b.as_object().map(|o| o.keys().cloned().collect())).unwrap_or_default();
    println!("  post hook input keys: {sample_keys:?}");
    p.quit();
}

/// 13. An http MCP server named claudiu: does Claude Code list and call its tools?
fn c13_mcp_http() {
    let server = HookServer::start(Arc::new(|path, body| {
        if path != "mcp" {
            return (Duration::ZERO, json!({}));
        }
        let id = body["id"].clone();
        let result = match body["method"].as_str() {
            Some("initialize") => json!({"protocolVersion": body["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "claudiu", "version": "0"}}),
            Some("tools/list") => json!({"tools": [{"name": "expand", "description": "Return kept-aside output by handle.", "inputSchema": {"type": "object", "properties": {"handle": {"type": "string"}}, "required": ["handle"]}}]}),
            Some("tools/call") => json!({"content": [{"type": "text", "text": format!("EXPANDED:{}", body["params"]["arguments"]["handle"])}]}),
            _ => return (Duration::ZERO, json!({"jsonrpc": "2.0", "id": id, "result": {}})),
        };
        (Duration::ZERO, json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }));
    let dir = scratch("c13");
    let (conn, rx) = Probe::spawn(&dir);
    Probe::init(&conn, false);
    let made = conn.call("session/new", json!({"cwd": dir.to_string_lossy(), "mcpServers": [{"type": "http", "name": "claudiu", "url": server.url("mcp"), "headers": []}], "_meta": Probe::meta(&Options::default())}), Duration::from_secs(90)).expect("session/new");
    let sid = made["sessionId"].as_str().unwrap().to_string();
    let _ = conn.call("session/set_mode", json!({"sessionId": sid, "modeId": "bypassPermissions"}), Duration::from_secs(30));
    let mut p = Probe { conn, rx, sid, dir, elicit: Box::new(|_| json!({"action": "cancel"})), elicitations: Vec::new(), made: Value::Null };
    let t = p.turn("Call the expand tool from the claudiu MCP server with handle abc123def456 and tell me exactly what it returned.");
    show("turn", &t);
    println!("  mcp requests seen by the server: {:?}", server.calls().iter().map(|c| c.body["method"].clone()).collect::<Vec<_>>());
    p.quit();
}

/// 14. UserPromptSubmit over http: does it fire on a prompt, and can it add context for the model?
fn c14_prompt_hook() {
    let started = Instant::now();
    let server = HookServer::start(Arc::new(|path, _| {
        if path == "prompt" {
            (Duration::ZERO, json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "The secret word is KIWI."}}))
        } else {
            (Duration::ZERO, json!({}))
        }
    }));
    let mut settings = json!({"disableAllHooks": false, "hooks": {}});
    settings["hooks"]["UserPromptSubmit"] = json!([{"hooks": [{"type": "http", "url": server.url("prompt"), "timeout": 20, "onFailure": "block"}]}]);
    settings["hooks"]["SessionStart"] = json!([{"hooks": [{"type": "http", "url": server.url("start"), "timeout": 20}]}]);
    let mut p = Probe::open("c14", Options { settings, ..Options::default() });
    println!("  session ready at {:.2}s; hook calls before the first prompt: {:?}", started.elapsed().as_secs_f64(), server.calls().iter().map(|c| c.path.clone()).collect::<Vec<_>>());
    let t = p.turn("What is the secret word? One word.");
    show("turn", &t);
    println!("  hook calls: {:?}", server.calls().iter().map(|c| (c.path.clone(), c.body["hook_event_name"].clone())).collect::<Vec<_>>());
    p.quit();
}

/// 15. A trimmed subagent session: which options does the adapter honour, and what does it offer to configure?
fn c15_subagent() {
    let options = json!({"tools": ["Read", "Grep", "Glob"], "settingSources": [], "strictMcpConfig": true, "promptSuggestions": false, "title": "scout: probe", "maxTurns": 5});
    let mut p = Probe::open("c15", Options { options, system_prompt: Some("You are a scout. Answer in one short line.".into()), ..Options::default() });
    let configs: Vec<String> = p.made["configOptions"].as_array().into_iter().flatten().map(|o| format!("{}={}", o["id"], o["currentValue"])).collect();
    println!("  config options offered: {}", configs.join(", "));
    for (id, value) in [("model", "haiku"), ("effort", "low")] {
        let r = p.conn.call("session/set_config_option", json!({"sessionId": p.sid, "configId": id, "value": value}), Duration::from_secs(30));
        println!("  set {id}={value}: {}", r.map_or_else(|e| format!("FAILED {e}"), |_| "ok".into()));
    }
    let t = p.turn("Name the tools you can call, comma separated, nothing else.");
    println!("  answer: {}", t.text.trim());
    if let Some((_, m)) = t.raw.iter().find(|(_, m)| m["type"] == "system" && m["subtype"] == "init") {
        println!("  init: model={} tools={} mcp={} permissionMode={}", m["model"], m["tools"], m["mcp_servers"], m["permissionMode"]);
    }
    let usage: Vec<String> = t.raw.iter().filter(|(_, m)| m["type"] == "assistant").map(|(_, m)| format!("{}:{}", m["message"]["id"], m["message"]["usage"])).collect();
    println!("  assistant usage messages: {}", usage.join(" | "));
}

/// 16. Where does the cost of a turn come from? Raw `result` messages over two turns.
fn c16_cost() {
    let mut p = Probe::open("c16", Options::default());
    for prompt in ["Say the word one.", "Say the word two."] {
        let t = p.turn(prompt);
        for (_, m) in t.raw.iter().filter(|(_, m)| m["type"] == "result") {
            println!("  result: subtype={} total_cost_usd={} num_turns={} usage={}", m["subtype"], m["total_cost_usd"], m["num_turns"], m["usage"]);
        }
        let costs: Vec<String> = t.updates.iter().filter(|u| u["sessionUpdate"] == "usage_update").map(|u| u.to_string()).collect();
        println!("  usage_update: {}", costs.join(" | "));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let checks: Vec<(&str, fn())> = vec![
        ("c1", c1_deny),
        ("c2", c2_rewrite),
        ("c3", c3_failure),
        ("c4", c4_latency),
        ("c5", c5_fork_cache),
        ("c6", c6_system_prompt),
        ("c7", c7_orphans),
        ("c8", c8_project_disable),
        ("c9", c9_questions),
        ("c10", c10_compaction),
        ("c11", c11_rewind),
        ("c12", c12_shapes),
        ("c13", c13_mcp_http),
        ("c14", c14_prompt_hook),
        ("c15", c15_subagent),
        ("c16", c16_cost),
    ];
    for (name, f) in checks {
        if args.is_empty() || args.iter().any(|a| a == name) {
            println!("== {name}");
            f();
        }
    }
}
