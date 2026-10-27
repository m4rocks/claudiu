//! A small Language Server Protocol client for editor mode: start a server found on PATH, talk to it over stdio, and
//! pass its diagnostics on. Nothing is downloaded or installed. No GPUI here;
//! `app::language_servers` wires it to tabs.

pub mod rpc;

use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use futures::channel::{mpsc as async_mpsc, oneshot};
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use lsp_types::{Diagnostic, Uri};
use serde_json::{Value, json};

/// A language server Claudiu knows how to start, and the editor languages (see `files::language`) it serves.
pub struct Kind {
    pub program: &'static str,
    args: &'static [&'static str],
    languages: &'static [&'static str],
    /// It gives diagnostics only when asked for them (`Server::diagnostics`), instead of sending them.
    pub pull_diagnostics: bool,
}

pub const KINDS: [Kind; 3] = [
    Kind { program: "rust-analyzer", args: &[], languages: &["rust"], pull_diagnostics: false },
    // TypeScript 7's own server. typescript-language-server drives the older JavaScript `tsserver`, which TypeScript 7
    // no longer ships, so it fails on projects that use it.
    Kind { program: "tsc", args: &["--lsp", "--stdio"], languages: &["typescript", "tsx", "javascript"], pull_diagnostics: true },
    Kind { program: "gleam", args: &["lsp"], languages: &["gleam"], pull_diagnostics: false },
];

pub fn kind_for(language: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.languages.contains(&language))
}

/// The LSP `languageId` for an editor language.
pub fn language_id(language: &str) -> &str {
    match language {
        "tsx" => "typescriptreact",
        other => other,
    }
}

/// A column on `line` counted in UTF-16 units, as most servers count, in characters, as the editor does. A column
/// inside a character (half of an emoji) moves past it.
pub fn utf16_to_chars(line: &str, units: u32) -> u32 {
    let mut seen = 0;
    for (chars, c) in line.chars().enumerate() {
        if seen >= units {
            return chars as u32;
        }
        seen += c.len_utf16() as u32;
    }
    line.chars().count() as u32
}

/// A column on `line` counted in characters, as the editor does, in UTF-16 units.
pub fn chars_to_utf16(line: &str, chars: u32) -> u32 {
    line.chars().take(chars as usize).map(|c| c.len_utf16() as u32).sum()
}

pub fn uri(path: &Path) -> Option<Uri> {
    url::Url::from_file_path(path).ok()?.as_str().parse().ok()
}

pub fn path_of(uri: &Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

/// What a server tells the UI without being asked.
#[derive(Debug)]
pub enum Event {
    Diagnostics(PathBuf, Vec<Diagnostic>),
    /// The server stopped on its own; the text is its last words on stderr, if any.
    Exited(String),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

/// A running language server. Dropping it asks the server to exit, then ends the process.
pub struct Server {
    outgoing: mpsc::Sender<Value>,
    pending: Pending,
    next_id: AtomicI64,
    child: Option<Child>,
    pull: bool,
    utf16: bool,
}

impl Server {
    /// Start `kind` in `root` and complete the `initialize` handshake. Blocks, so call it off the UI thread.
    pub fn start(kind: &Kind, program: &Path, root: &Path, events: async_mpsc::UnboundedSender<Event>) -> Result<Server, String> {
        let mut cmd = Command::new(program);
        cmd.args(kind.args).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::platform::no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", kind.program))?;
        let (stdin, stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
        match Self::connect(stdout, stdin, Some(stderr), root, kind.pull_diagnostics, events) {
            Ok(mut server) => {
                server.child = Some(child);
                Ok(server)
            }
            // Dropping a `Child` leaves the process running, so a server stuck in the handshake would outlive the pane.
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }

    /// Talk to a server over `reader` and `writer` (its stdout and stdin) and complete the handshake. `pull`: ask it for
    /// diagnostics (see `Kind::pull_diagnostics`); offered only to servers that need it, since a server that does both may
    /// then send fewer.
    pub fn connect(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        stderr: Option<impl Read + Send + 'static>,
        root: &Path,
        pull: bool,
        events: async_mpsc::UnboundedSender<Event>,
    ) -> Result<Server, String> {
        let (outgoing, queue) = mpsc::channel::<Value>();
        std::thread::spawn(move || {
            let mut writer = BufWriter::new(writer);
            while let Ok(message) = queue.recv() {
                if rpc::write_message(&mut writer, &message).is_err() {
                    break;
                }
            }
        });
        // Kept only to explain a crash; never logged.
        let last_words = Arc::new(Mutex::new(VecDeque::<String>::new()));
        if let Some(stderr) = stderr {
            let last_words = last_words.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut lines = last_words.lock().unwrap();
                    lines.push_back(line);
                    if lines.len() > 3 {
                        lines.pop_front();
                    }
                }
            });
        }
        let pending: Pending = Default::default();
        {
            let (pending, outgoing) = (pending.clone(), outgoing.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(reader);
                while let Ok(Some(message)) = rpc::read_message(&mut reader) {
                    handle(message, &pending, &outgoing, &events);
                }
                pending.lock().unwrap().clear();
                // Give stderr a moment to deliver the reason before reporting it.
                std::thread::sleep(Duration::from_millis(200));
                let words = last_words.lock().unwrap().iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
                let _ = events.unbounded_send(Event::Exited(words));
            });
        }
        let mut server = Server { outgoing, pending, next_id: AtomicI64::new(1), child: None, pull, utf16: true };
        let mut params = json!({
            "processId": std::process::id(),
            "rootUri": uri(root).map(|u| u.as_str().to_string()),
            "workspaceFolders": [{"uri": uri(root).map(|u| u.as_str().to_string()), "name": root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()}],
            "capabilities": {
                // The editor counts columns in characters; a server that keeps to UTF-16 has its columns converted.
                "general": {"positionEncodings": ["utf-32", "utf-16"]},
                "textDocument": {
                    "synchronization": {"didSave": true},
                    "publishDiagnostics": {},
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {"linkSupport": true},
                    // Plain text only: the editor inserts an item's text as it comes, so no snippets.
                    "completion": {"contextSupport": true, "completionItem": {"snippetSupport": false}},
                },
            },
        });
        if pull {
            params["capabilities"]["textDocument"]["diagnostic"] = json!({});
        }
        let reply = server.send_request(lsp_types::request::Initialize::METHOD, params);
        let (done, answered) = mpsc::channel();
        std::thread::spawn(move || done.send(futures::executor::block_on(reply)));
        let initialized = answered
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| Err("no answer within 30 seconds".into()))
            .map_err(|e| format!("the handshake failed: {e}"))?;
        // A server that names no encoding uses UTF-16, as the protocol says.
        server.utf16 = initialized["capabilities"]["positionEncoding"].as_str() != Some("utf-32");
        server.notify::<lsp_types::notification::Initialized>(lsp_types::InitializedParams {});
        Ok(server)
    }

    fn send_request(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, String>> + use<> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _ = self.outgoing.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        async move { rx.await.unwrap_or_else(|_| Err("the server stopped".into())) }
    }

    pub fn request<R: Request>(&self, params: R::Params) -> impl Future<Output = Result<R::Result, String>> + use<R> {
        let reply = self.send_request(R::METHOD, serde_json::to_value(params).unwrap_or(Value::Null));
        async move { serde_json::from_value(reply.await?).map_err(|e| e.to_string()) }
    }

    /// Whether the server counts columns in UTF-16 units rather than characters (see `utf16_to_chars`).
    pub fn counts_utf16(&self) -> bool {
        self.utf16
    }

    /// Whether diagnostics come from `diagnostics` rather than `Event::Diagnostics`.
    pub fn pulls_diagnostics(&self) -> bool {
        self.pull
    }

    /// Ask for a document's diagnostics. `None` when they haven't changed since the last answer.
    pub fn diagnostics(&self, uri: &Uri) -> impl Future<Output = Result<Option<Vec<Diagnostic>>, String>> + use<> {
        let reply = self.send_request(lsp_types::request::DocumentDiagnosticRequest::METHOD, json!({"textDocument": {"uri": uri.as_str()}}));
        async move { full_report(reply.await?) }
    }

    pub fn notify<N: Notification>(&self, params: N::Params) {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        let _ = self.outgoing.send(json!({"jsonrpc": "2.0", "method": N::METHOD, "params": params}));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // The reply isn't waited for: the server gets `exit` right after, and the process is ended if it lingers.
        drop(self.send_request(lsp_types::request::Shutdown::METHOD, Value::Null));
        self.notify::<lsp_types::notification::Exit>(());
        if let Some(mut child) = self.child.take() {
            // A polite server is gone well within this; one that isn't gets ended, off the UI thread.
            std::thread::spawn(move || {
                for _ in 0..20 {
                    if child.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// The items of a full diagnostic report; an `unchanged` one has none.
fn full_report(reply: Value) -> Result<Option<Vec<Diagnostic>>, String> {
    match reply.get("kind").and_then(Value::as_str) {
        Some("full") => serde_json::from_value(reply["items"].clone()).map(Some).map_err(|e| e.to_string()),
        _ => Ok(None),
    }
}

fn handle(message: Value, pending: &Pending, outgoing: &mpsc::Sender<Value>, events: &async_mpsc::UnboundedSender<Event>) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    match (method, id) {
        // A reply to one of ours.
        (None, Some(id)) => {
            let Some(tx) = id.as_i64().and_then(|id| pending.lock().unwrap().remove(&id)) else { return };
            let reply = match message.get("error") {
                Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
        }
        // The server asks something; answer every request so none waits forever. Settings are left at its defaults.
        (Some(method), Some(id)) => {
            let result = match method {
                "workspace/configuration" => {
                    let n = message.pointer("/params/items").and_then(Value::as_array).map_or(0, Vec::len);
                    Value::Array(vec![Value::Null; n])
                }
                _ => Value::Null,
            };
            let _ = outgoing.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
        }
        (Some(lsp_types::notification::PublishDiagnostics::METHOD), None) => {
            let Ok(params) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(message["params"].clone()) else { return };
            if let Some(path) = path_of(&params.uri) {
                let _ = events.unbounded_send(Event::Diagnostics(path, params.diagnostics));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::io::BufReader;

    /// A fake server on the other end of two pipes: it answers `initialize`, asks for configuration, publishes a
    /// diagnostic when a document opens, answers hover and a request for diagnostics, and stops when told.
    fn fake_server(from_client: std::io::PipeReader, mut to_client: std::io::PipeWriter, root: PathBuf) {
        std::thread::spawn(move || {
            let mut input = BufReader::new(from_client);
            while let Ok(Some(m)) = rpc::read_message(&mut input) {
                let reply = |out: &mut std::io::PipeWriter, result: Value| rpc::write_message(out, &json!({"jsonrpc": "2.0", "id": m["id"], "result": result})).unwrap();
                match m["method"].as_str() {
                    Some("initialize") => {
                        assert_eq!(m["params"]["capabilities"]["textDocument"]["diagnostic"], json!({}), "pull diagnostics are offered");
                        assert_eq!(m["params"]["capabilities"]["textDocument"]["completion"]["completionItem"]["snippetSupport"], json!(false), "snippets are declined");
                        rpc::write_message(&mut to_client, &json!({"jsonrpc": "2.0", "id": "cfg", "method": "workspace/configuration", "params": {"items": [{}, {}]}})).unwrap();
                        reply(&mut to_client, json!({"capabilities": {}}));
                    }
                    Some("textDocument/didOpen") => {
                        let uri = uri(&root.join("main.rs")).unwrap();
                        let d = json!({"uri": uri.as_str(), "diagnostics": [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}, "message": "unused", "severity": 2}]});
                        rpc::write_message(&mut to_client, &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": d})).unwrap();
                    }
                    Some("textDocument/hover") => reply(&mut to_client, json!({"contents": {"kind": "markdown", "value": "fn main()"}})),
                    Some("textDocument/diagnostic") => {
                        assert_eq!(m["params"]["textDocument"]["uri"], uri(&root.join("main.rs")).unwrap().as_str());
                        let d = json!({"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}}, "message": "asked for"});
                        reply(&mut to_client, json!({"kind": "full", "items": [d]}));
                    }
                    Some("shutdown") => reply(&mut to_client, Value::Null),
                    Some("exit") => return,
                    _ => {}
                }
                if m["id"] == "cfg" {
                    assert_eq!(m["result"], json!([null, null]), "configuration is answered item by item");
                }
            }
        });
    }

    #[test]
    fn handshake_diagnostics_hover_and_exit() {
        let root = std::env::temp_dir();
        let (server_in, client_out) = std::io::pipe().unwrap();
        let (client_in, server_out) = std::io::pipe().unwrap();
        fake_server(server_in, server_out, root.clone());
        let (tx, mut rx) = async_mpsc::unbounded();
        let server = Server::connect(client_in, client_out, None::<std::io::Empty>, &root, true, tx).unwrap();
        assert!(server.counts_utf16(), "a server that names no encoding counts in UTF-16");
        let doc = uri(&root.join("main.rs")).unwrap();
        server.notify::<lsp_types::notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem::new(doc.clone(), "rust".into(), 0, "fn main() {}".into()),
        });
        match futures::executor::block_on(rx.next()) {
            Some(Event::Diagnostics(path, d)) => {
                assert_eq!(path, root.join("main.rs"));
                assert_eq!((d.len(), d[0].message.as_str()), (1, "unused"));
            }
            other => panic!("expected diagnostics, got {other:?}"),
        }
        let hover = server.request::<lsp_types::request::HoverRequest>(lsp_types::HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams::new(lsp_types::TextDocumentIdentifier::new(doc.clone()), lsp_types::Position::new(0, 4)),
            work_done_progress_params: Default::default(),
        });
        let hover = futures::executor::block_on(hover).unwrap().unwrap();
        assert!(matches!(hover.contents, lsp_types::HoverContents::Markup(m) if m.value == "fn main()"));
        let pulled = futures::executor::block_on(server.diagnostics(&doc)).unwrap().unwrap();
        assert_eq!(pulled.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(), ["asked for"]);
        drop(server);
        assert!(matches!(futures::executor::block_on(rx.next()), Some(Event::Exited(_))), "the end of the server is reported");
    }

    #[test]
    fn columns_convert_between_characters_and_utf16() {
        let line = "let 😀 = é;";
        // Characters: l e t ␠ 😀 ␠ = ␠ é ;  — the emoji is one character but two UTF-16 units.
        assert_eq!(chars_to_utf16(line, 4), 4, "before the emoji");
        assert_eq!(chars_to_utf16(line, 5), 6, "after the emoji");
        assert_eq!(chars_to_utf16(line, 9), 10, "é is one unit");
        assert_eq!(utf16_to_chars(line, 6), 5);
        assert_eq!(utf16_to_chars(line, 10), 9);
        assert_eq!(utf16_to_chars(line, 5), 5, "inside the emoji moves past it");
        assert_eq!(utf16_to_chars(line, 99), 10, "past the end stays at the end");
        for chars in 0..=10 {
            assert_eq!(utf16_to_chars(line, chars_to_utf16(line, chars)), chars, "round trip at {chars}");
        }
    }

    #[test]
    fn languages_map_to_servers() {
        assert_eq!(kind_for("rust").map(|k| k.program), Some("rust-analyzer"));
        assert_eq!(kind_for("tsx").map(|k| k.program), Some("tsc"));
        assert_eq!(kind_for("javascript").map(|k| k.program), Some("tsc"));
        assert_eq!(kind_for("gleam").map(|k| k.program), Some("gleam"));
        assert!(kind_for("markdown").is_none());
        assert_eq!(language_id("tsx"), "typescriptreact");
        let p = std::env::temp_dir().join("a b.rs");
        assert_eq!(path_of(&uri(&p).unwrap()), Some(p), "paths with spaces survive the round trip");
    }
}
