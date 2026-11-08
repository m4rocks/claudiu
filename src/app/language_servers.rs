//! Language servers for editor mode: one per kind (see `lsp::KINDS`) per session, started when the first file of its
//! language opens and ended with the session. A missing or stopped server is shown, never installed or restarted.
//! Requests run off the UI thread; their answers go to the editor that asked (`editor::assist`).

use std::path::Path;
use std::sync::Arc;

use futures::StreamExt;
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument};
use lsp_types::request::{Completion, GotoDefinition, HoverRequest};
use lsp_types::{CompletionResponse, CompletionTextEdit, GotoDefinitionResponse, HoverContents, MarkedString, TextDocumentIdentifier, TextDocumentPositionParams};

use super::*;
use super::editor_mode::Tab;
use crate::editor::assist::{Item, Request};
use crate::editor::buffer::{Pos, Selection};
use crate::editor::{Editor, Problem};
use crate::lsp::{self, Event, Server};
use crate::platform::path_key;

/// Edits are sent to the server once typing pauses this long.
const SYNC_DELAY: Duration = Duration::from_millis(300);
/// Most completions kept from one answer.
const MOST_COMPLETIONS: usize = 400;

pub enum ServerState {
    Starting,
    Ready(Arc<Server>),
    /// Not found on PATH.
    Missing,
    /// Failed to start or stopped on its own, with why.
    Stopped(String),
}

impl Workspace {
    /// Hand a newly opened tab to its language's server, starting the server on first use.
    pub(super) fn attach_server(&mut self, id: &Id, path: &Path) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        let Some(kind) = pane.tabs.iter().find(|t| t.path == path).and_then(|t| t.language).and_then(lsp::kind_for) else { return };
        match pane.servers.get(kind.program) {
            Some(ServerState::Ready(server)) => {
                let server = server.clone();
                self.open_in_server(id, path, &server);
            }
            Some(_) => {}
            None => {
                pane.servers.insert(kind.program, ServerState::Starting);
                self.start_server(id, kind);
            }
        }
    }

    fn start_server(&mut self, id: &Id, kind: &'static lsp::Kind) {
        let Some(root) = self.store.session(id).map(|s| s.cwd.clone()) else { return };
        let mailbox = self.rt.mailbox();
        let owner = id.clone();
        self.rt.spawn(
            move || {
                let program = crate::platform::find_executable(kind.program).ok_or(None)?;
                let (events, incoming) = futures::channel::mpsc::unbounded();
                let server = Server::start(kind, &program, &root, events).map_err(Some)?;
                forward_events(mailbox, owner, kind.program, incoming);
                Ok(server)
            },
            {
                let id = id.clone();
                move |ws: &mut Workspace, started: Result<Server, Option<String>>| ws.on_server_started(&id, kind, started)
            },
        );
    }

    pub(super) fn on_server_started(&mut self, id: &Id, kind: &'static lsp::Kind, started: Result<Server, Option<String>>) {
        // The session ended while its server started: dropping the server ends it too.
        let Some(pane) = self.panes.get_mut(id) else { return };
        let state = match started {
            Ok(server) => ServerState::Ready(Arc::new(server)),
            Err(None) => ServerState::Missing,
            Err(Some(why)) => ServerState::Stopped(why),
        };
        pane.servers.insert(kind.program, state);
        if let Some(ServerState::Ready(server)) = pane.servers.get(kind.program) {
            let server = server.clone();
            let paths: Vec<PathBuf> = pane.tabs.iter().filter(|t| t.language.and_then(lsp::kind_for).is_some_and(|k| k.program == kind.program)).map(|t| t.path.clone()).collect();
            for path in paths {
                self.open_in_server(id, &path, &server);
            }
        }
    }

    fn on_server_event(&mut self, id: &Id, program: &'static str, event: Event) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        match event {
            Event::Diagnostics(path, diagnostics) => {
                let key = path_key(&path);
                if let Some(tab) = pane.tabs.iter_mut().find(|t| path_key(&t.path) == key) {
                    let utf16 = tab.server.as_ref().is_some_and(|s| s.counts_utf16());
                    show_diagnostics(&mut tab.editor, &diagnostics, utf16);
                }
            }
            Event::Exited(why) => {
                let why = if why.is_empty() { "it exited".to_string() } else { why };
                pane.servers.insert(program, ServerState::Stopped(why));
                for tab in pane.tabs.iter_mut().filter(|t| t.language.and_then(lsp::kind_for).is_some_and(|k| k.program == program)) {
                    tab.server = None;
                }
            }
        }
    }

    /// Tell the server about an open tab, so it can answer about it.
    fn open_in_server(&mut self, id: &Id, path: &Path, server: &Arc<Server>) {
        let Some(tab) = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
        let (Some(uri), Some(language)) = (lsp::uri(path), tab.language) else { return };
        server.notify::<DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem::new(uri, lsp::language_id(language).into(), tab.version, tab.editor.buffer.text()),
        });
        tab.server = Some(server.clone());
        self.pull_diagnostics(id, path);
    }

    /// The tab's text changed: send it to the server once typing pauses (`immediately` for reloads).
    pub(super) fn sync_with_server(&mut self, id: &Id, path: &Path, immediately: bool) {
        let Some(tab) = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
        if tab.server.is_none() {
            return;
        }
        tab.version += 1;
        let version = tab.version;
        let (id, path) = (id.clone(), path.to_path_buf());
        let send = move |ws: &mut Workspace| {
            let Some(tab) = ws.panes.get(&id).and_then(|p| p.tabs.iter().find(|t| t.path == path)) else { return };
            // A later edit has its own pending send.
            let (Some(server), Some(uri), true) = (&tab.server, lsp::uri(&path), tab.version == version) else { return };
            server.notify::<DidChangeTextDocument>(lsp_types::DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier::new(uri, version),
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent { range: None, range_length: None, text: tab.editor.buffer.text() }],
            });
            ws.pull_diagnostics(&id, &path);
        };
        if immediately {
            send(self);
        } else {
            self.rt.after(SYNC_DELAY, send);
        }
    }

    /// Send the tab's text now, for a request that must be answered about what is on screen; `sync_with_server` waits
    /// for typing to pause.
    fn send_text_now(&mut self, id: &Id, path: &Path) {
        self.sync_with_server(id, path, true);
    }

    /// Ask a server that doesn't send diagnostics for the tab's (see `lsp::Kind::pull_diagnostics`).
    fn pull_diagnostics(&mut self, id: &Id, path: &Path) {
        let Some(tab) = self.panes.get(id).and_then(|p| p.tabs.iter().find(|t| t.path == path)) else { return };
        let (Some(server), Some(uri)) = (tab.server.as_ref().filter(|s| s.pulls_diagnostics()), lsp::uri(path)) else { return };
        let (reply, version, utf16) = (server.diagnostics(&uri), tab.version, server.counts_utf16());
        let (id, path) = (id.clone(), path.to_path_buf());
        self.rt.spawn(
            move || futures::executor::block_on(reply),
            move |ws: &mut Workspace, reply| {
                let Ok(Some(diagnostics)) = reply else { return };
                // A later edit asks again; this answer is about older text.
                let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)).filter(|t| t.version == version) else { return };
                show_diagnostics(&mut tab.editor, &diagnostics, utf16);
            },
        );
    }

    pub(super) fn tell_server_saved(&self, id: &Id, path: &Path) {
        let Some(tab) = self.panes.get(id).and_then(|p| p.tabs.iter().find(|t| t.path == path)) else { return };
        if let (Some(server), Some(uri)) = (&tab.server, lsp::uri(path)) {
            server.notify::<DidSaveTextDocument>(lsp_types::DidSaveTextDocumentParams { text_document: TextDocumentIdentifier::new(uri), text: None });
        }
    }

    pub(super) fn tell_server_closed(tab: &Tab) {
        if let (Some(server), Some(uri)) = (&tab.server, lsp::uri(&tab.path)) {
            server.notify::<DidCloseTextDocument>(lsp_types::DidCloseTextDocumentParams { text_document: TextDocumentIdentifier::new(uri) });
        }
    }

    /// One line about the shown tab's server, when its language has one.
    pub fn server_status(&self, id: &Id) -> Option<String> {
        let pane = self.panes.get(id)?;
        let kind = pane.shown()?.language.and_then(lsp::kind_for)?;
        let name = kind.program;
        Some(match pane.servers.get(name)? {
            ServerState::Starting => format!("{name} is starting…"),
            ServerState::Ready(_) => name.to_string(),
            ServerState::Missing => format!("{name} isn't installed, so this file has no diagnostics or hover."),
            ServerState::Stopped(why) => format!("{name} stopped: {why}"),
        })
    }

    /// Answer what an editor asked for (`Editor::requests`).
    pub(super) fn serve_editor(&mut self, key: &str, request: Request) {
        let Some((id, path)) = self.panes.iter().find_map(|(id, p)| p.tabs.iter().find(|t| t.editor.key == key).map(|t| (id.clone(), t.path.clone()))) else { return };
        match request {
            Request::Hover(at, serial) => {
                let key = key.to_string();
                self.rt.after(crate::editor::assist::HOVER_DELAY, move |ws: &mut Workspace| ws.hover(&key, &id, &path, at, serial));
            }
            Request::Definition(at) => self.definition(&id, &path, at),
            Request::Complete(at) => self.complete(&id, &path, at),
        }
    }

    /// The pointer rested on the word at `at`: show its problems at once, and what the server says once it answers.
    fn hover(&mut self, key: &str, id: &Id, path: &Path, at: Pos, serial: u64) {
        let Some(tab) = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
        if !tab.editor.hover_wanted(serial) {
            return;
        }
        tab.editor.show_hover(serial, at, None);
        let (Some(server), Some(uri)) = (tab.server.clone(), lsp::uri(path)) else { return };
        let params = position(&tab.editor, &server, uri, at);
        let reply = server.request::<HoverRequest>(lsp_types::HoverParams { text_document_position_params: params, work_done_progress_params: Default::default() });
        let key = key.to_string();
        self.rt.spawn(
            move || futures::executor::block_on(reply),
            move |ws: &mut Workspace, reply| {
                let docs = reply.ok().flatten().map(|h| hover_markdown(h.contents)).filter(|d| !d.trim().is_empty());
                if let Some(e) = ws.editor_mut(&key)
                    && docs.is_some()
                {
                    e.show_hover(serial, at, docs);
                }
            },
        );
    }

    /// F12 or Cmd+click: go to where the thing at `at` is defined, in this file or another.
    fn definition(&mut self, id: &Id, path: &Path, at: Pos) {
        self.send_text_now(id, path);
        let Some(tab) = self.panes.get(id).and_then(|p| p.tabs.iter().find(|t| t.path == path)) else { return };
        let (Some(server), Some(uri)) = (tab.server.clone(), lsp::uri(path)) else {
            self.toast("This file has no language server to find definitions with.", false);
            return;
        };
        let params = position(&tab.editor, &server, uri, at);
        let reply = server.request::<GotoDefinition>(lsp_types::GotoDefinitionParams {
            text_document_position_params: params,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        let utf16 = server.counts_utf16();
        let (id, path) = (id.clone(), path.to_path_buf());
        self.rt.spawn(
            move || futures::executor::block_on(reply),
            move |ws: &mut Workspace, reply| {
                let Some((target, place)) = reply.ok().and_then(first_location) else {
                    ws.toast("No definition found here.", false);
                    return;
                };
                let Some(target) = lsp::path_of(&target) else { return };
                // In the tree's form (a server may write `c:` for `C:`), so an open tab of the file is found by its path.
                let target = dunce::canonicalize(&target).unwrap_or(target);
                if path_key(&target) != path_key(&path) {
                    ws.open_file(&id, target, Some(place.line as usize));
                    return;
                }
                ws.remember_place(&id, &path, Some(place.line as usize));
                let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
                let line = place.line as usize;
                let col = from_lsp(tab.editor.buffer.line(line), place.character, utf16);
                tab.editor.go_to_line(line);
                tab.editor.select(Selection::caret(Pos::new(line, col)));
            },
        );
    }

    /// Ctrl+Space: the server's completions for the word at `at`.
    fn complete(&mut self, id: &Id, path: &Path, at: Pos) {
        self.send_text_now(id, path);
        let Some(tab) = self.panes.get(id).and_then(|p| p.tabs.iter().find(|t| t.path == path)) else { return };
        let (Some(server), Some(uri)) = (tab.server.clone(), lsp::uri(path)) else { return };
        let params = position(&tab.editor, &server, uri, at);
        let reply = server.request::<Completion>(lsp_types::CompletionParams {
            text_document_position: params,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            // Every request is the user's own; the editor filters the answer by the word typed.
            context: Some(lsp_types::CompletionContext { trigger_kind: lsp_types::CompletionTriggerKind::INVOKED, trigger_character: None }),
        });
        let (utf16, key) = (server.counts_utf16(), tab.editor.key.clone());
        self.rt.spawn(
            move || futures::executor::block_on(reply),
            move |ws: &mut Workspace, reply| {
                let Some(e) = ws.editor_mut(&key) else { return };
                let items = completion_items(reply.ok().flatten(), utf16, &|line| e.buffer.line(line).to_string());
                e.show_completions(at, items);
            },
        );
    }
}

/// Pass what the server says unasked to the session's pane, until the server ends or the app does.
pub(super) fn forward_events(mailbox: rt::Mailbox<Workspace>, owner: Id, program: &'static str, mut incoming: futures::channel::mpsc::UnboundedReceiver<Event>) {
    std::thread::spawn(move || {
        while let Some(event) = futures::executor::block_on(incoming.next()) {
            let owner = owner.clone();
            if !mailbox.send(move |ws: &mut Workspace| ws.on_server_event(&owner, program, event)) {
                break;
            }
        }
    });
}

/// `at` in the editor as the server counts it.
fn position(e: &Editor, server: &Server, uri: lsp_types::Uri, at: Pos) -> TextDocumentPositionParams {
    let character = to_lsp(e.buffer.line(at.line), at.col, server.counts_utf16());
    TextDocumentPositionParams::new(TextDocumentIdentifier::new(uri), lsp_types::Position::new(at.line as u32, character))
}

/// A byte column on `line` as a server counts it: in UTF-16 units, or in characters.
fn to_lsp(line: &str, col: usize, utf16: bool) -> u32 {
    let before = &line[..col.min(line.len())];
    if utf16 { before.encode_utf16().count() as u32 } else { before.chars().count() as u32 }
}

/// A server's column on `line` as a byte column; one inside a character (half an emoji) moves past it.
fn from_lsp(line: &str, character: u32, utf16: bool) -> usize {
    let mut seen = 0;
    for (i, c) in line.char_indices() {
        if seen >= character {
            return i;
        }
        seen += if utf16 { c.len_utf16() as u32 } else { 1 };
    }
    line.len()
}

fn to_pos(p: lsp_types::Position, line_of: &dyn Fn(usize) -> String, utf16: bool) -> Pos {
    let line = p.line as usize;
    Pos::new(line, from_lsp(&line_of(line), p.character, utf16))
}

/// The server's problems as the editor underlines them.
fn show_diagnostics(e: &mut Editor, diagnostics: &[lsp_types::Diagnostic], utf16: bool) {
    let line_of = |l: usize| e.buffer.line(l).to_string();
    let problems: Vec<Problem> = diagnostics
        .iter()
        .map(|d| Problem {
            from: e.buffer.clamp(to_pos(d.range.start, &line_of, utf16)),
            to: e.buffer.clamp(to_pos(d.range.end, &line_of, utf16)),
            error: d.severity.is_none_or(|s| s == lsp_types::DiagnosticSeverity::ERROR),
            message: d.message.clone(),
        })
        .collect();
    e.problems = problems;
    e.dirty = true;
}

/// A hover's contents as Markdown, code in fenced blocks.
fn hover_markdown(contents: HoverContents) -> String {
    let marked = |m: MarkedString| match m {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(l) => format!("```{}\n{}\n```", l.language, l.value),
    };
    match contents {
        HoverContents::Scalar(m) => marked(m),
        HoverContents::Array(ms) => ms.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
        HoverContents::Markup(m) => m.value,
    }
}

/// The first place a definition answer names: its file and where in it.
fn first_location(reply: Option<GotoDefinitionResponse>) -> Option<(lsp_types::Uri, lsp_types::Position)> {
    match reply? {
        GotoDefinitionResponse::Scalar(l) => Some((l.uri, l.range.start)),
        GotoDefinitionResponse::Array(ls) => ls.into_iter().next().map(|l| (l.uri, l.range.start)),
        GotoDefinitionResponse::Link(ls) => ls.into_iter().next().map(|l| (l.target_uri, l.target_selection_range.start)),
    }
}

/// A server's completions as the editor lists them, with their edits' columns in bytes as the editor counts them.
fn completion_items(reply: Option<CompletionResponse>, utf16: bool, line_of: &dyn Fn(usize) -> String) -> Vec<Item> {
    let items = match reply {
        None => vec![],
        Some(CompletionResponse::Array(items)) => items,
        Some(CompletionResponse::List(list)) => list.items,
    };
    items
        .into_iter()
        .take(MOST_COMPLETIONS)
        .map(|item| {
            let range = |r: lsp_types::Range| (to_pos(r.start, line_of, utf16), to_pos(r.end, line_of, utf16));
            let (insert, replace) = match item.text_edit {
                Some(CompletionTextEdit::Edit(edit)) => (edit.new_text, Some(range(edit.range))),
                Some(CompletionTextEdit::InsertAndReplace(edit)) => (edit.new_text, Some(range(edit.replace))),
                None => (item.insert_text.unwrap_or_else(|| item.label.clone()), None),
            };
            Item { label: item.label, detail: item.detail, insert, replace }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{CompletionItem, Position, Range, TextEdit};

    #[test]
    fn columns_convert_between_bytes_and_what_the_server_counts() {
        let line = "let 😀 = é;";
        // Bytes: the emoji is four, é is two. UTF-16: the emoji is two units, é one. Characters: one each.
        assert_eq!(to_lsp(line, 4, true), 4, "before the emoji");
        assert_eq!(to_lsp(line, 8, true), 6, "after the emoji");
        assert_eq!(to_lsp(line, 8, false), 5);
        assert_eq!(from_lsp(line, 6, true), 8);
        assert_eq!(from_lsp(line, 5, false), 8);
        assert_eq!(from_lsp(line, 5, true), 8, "inside the emoji moves past it");
        assert_eq!(from_lsp(line, 99, true), line.len(), "past the end stays at the end");
        for (col, _) in line.char_indices() {
            assert_eq!(from_lsp(line, to_lsp(line, col, true), true), col, "round trip at {col}");
        }
    }

    #[test]
    fn completion_edits_land_on_the_bytes_they_name() {
        // "ab" starts at byte 8 but UTF-16 unit 6, because the emoji is four bytes and two units.
        let text = "let 😀ab = 1;";
        let item = CompletionItem { label: "abc".into(), text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(Range::new(Position::new(0, 6), Position::new(0, 8)), "abc".into()))), ..Default::default() };
        let items = completion_items(Some(CompletionResponse::Array(vec![item])), true, &|_| text.to_string());
        assert_eq!(items[0].replace, Some((Pos::new(0, 8), Pos::new(0, 10))));
        let plain = CompletionItem { label: "x".into(), ..Default::default() };
        let list = lsp_types::CompletionList { is_incomplete: false, items: vec![plain] };
        assert_eq!(completion_items(Some(CompletionResponse::List(list)), true, &|_| String::new())[0].insert, "x");
        assert!(completion_items(None, true, &|_| String::new()).is_empty());
    }

    #[test]
    fn hover_code_becomes_a_fenced_block() {
        let c = HoverContents::Array(vec![MarkedString::LanguageString(lsp_types::LanguageString { language: "rust".into(), value: "fn a()".into() }), MarkedString::String("Docs.".into())]);
        assert_eq!(hover_markdown(c), "```rust\nfn a()\n```\n\nDocs.");
    }
}
