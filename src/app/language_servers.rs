//! Language servers for editor mode: one per kind (see `lsp::KINDS`) per session, started when the first file of its
//! language opens and ended with the session. A missing or stopped server is shown, never installed or restarted.

use std::path::Path;
use std::rc::Rc;

use futures::StreamExt;
use gpui_kit::component::input::{DefinitionProvider, HoverProvider, Rope, RopeExt};
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument};
use lsp_types::request::{GotoDefinition, HoverRequest};
use lsp_types::{GotoDefinitionResponse, LocationLink, TextDocumentIdentifier, TextDocumentPositionParams, Uri};

use super::*;
use crate::lsp::{self, Event, Server};
use crate::platform::path_key;

/// Edits are sent to the server once typing pauses this long.
const SYNC_DELAY: Duration = Duration::from_millis(300);

pub enum ServerState {
    Starting,
    Ready(Rc<Server>),
    /// Not found on PATH.
    Missing,
    /// Failed to start or stopped on its own, with why.
    Stopped(String),
}

impl Workspace {
    /// Hand a newly opened tab to its language's server, starting the server on first use.
    pub(super) fn attach_server(&mut self, id: &Id, path: &Path, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        let Some(kind) = pane.tabs.iter().find(|t| t.path == path).and_then(|t| t.language).and_then(lsp::kind_for) else { return };
        match pane.servers.get(kind.program) {
            Some(ServerState::Ready(server)) => {
                let server = server.clone();
                self.open_in_server(id, path, &server, cx);
            }
            Some(_) => {}
            None => {
                pane.servers.insert(kind.program, ServerState::Starting);
                self.start_server(id, kind, cx);
            }
        }
    }

    fn start_server(&mut self, id: &Id, kind: &'static lsp::Kind, cx: &mut Context<Self>) {
        let Some(root) = self.store.session(id).map(|s| s.cwd.clone()) else { return };
        let (events, mut incoming) = futures::channel::mpsc::unbounded();
        let id = id.clone();
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    let program = crate::platform::find_executable(kind.program).ok_or(None)?;
                    Server::start(kind, &program, &root, events).map_err(Some)
                })
                .await;
            let ready = this.update(cx, |ws, cx| {
                let Some(pane) = ws.panes.get_mut(&id) else { return false };
                let state = match started {
                    Ok(server) => ServerState::Ready(Rc::new(server)),
                    Err(None) => ServerState::Missing,
                    Err(Some(why)) => ServerState::Stopped(why),
                };
                let ready = matches!(state, ServerState::Ready(_));
                pane.servers.insert(kind.program, state);
                if let Some(ServerState::Ready(server)) = pane.servers.get(kind.program) {
                    let server = server.clone();
                    let paths: Vec<PathBuf> = pane.tabs.iter().filter(|t| t.language.and_then(lsp::kind_for).is_some_and(|k| k.program == kind.program)).map(|t| t.path.clone()).collect();
                    for path in paths {
                        ws.open_in_server(&id, &path, &server, cx);
                    }
                }
                cx.notify();
                ready
            });
            if !matches!(ready, Ok(true)) {
                return;
            }
            while let Some(event) = incoming.next().await {
                if this.update(cx, |ws, cx| ws.on_server_event(&id, kind.program, event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_server_event(&mut self, id: &Id, program: &'static str, event: Event, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        match event {
            Event::Diagnostics(path, diagnostics) => {
                let key = path_key(&path);
                if let Some(tab) = pane.tabs.iter().find(|t| path_key(&t.path) == key) {
                    show_diagnostics(tab, diagnostics, cx);
                }
            }
            Event::Exited(why) => {
                let why = if why.is_empty() { "it exited".to_string() } else { why };
                pane.servers.insert(program, ServerState::Stopped(why));
                cx.notify();
            }
        }
    }

    /// Tell the server about an open tab, and let the tab ask it for hover and definitions.
    fn open_in_server(&mut self, id: &Id, path: &Path, server: &Rc<Server>, cx: &mut Context<Self>) {
        let ws = cx.entity().downgrade();
        let Some(tab) = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
        let (Some(uri), Some(language)) = (lsp::uri(path), tab.language) else { return };
        let text = tab.buffer.read(cx).value().to_string();
        server.notify::<DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem::new(uri.clone(), lsp::language_id(language).into(), tab.version, text),
        });
        tab.server = Some(server.clone());
        let doc = Rc::new(Document { server: server.clone(), uri });
        let (owner, current) = (id.clone(), path.to_path_buf());
        tab.buffer.update(cx, |b, _| {
            let lsp = b.lsp_mut();
            lsp.hover_provider = Some(doc.clone());
            lsp.definition_provider = Some(doc);
            // A definition in another file opens that file's tab; one in this file is a plain jump.
            lsp.show_document = Some(Rc::new(move |params, window, cx| {
                let Some(target) = lsp::path_of(&params.uri).filter(|t| path_key(t) != path_key(&current)) else { return false };
                // In the tree's form (a server may write `c:` for `C:`), so an open tab of the file is found by its path.
                let target = dunce::canonicalize(&target).unwrap_or(target);
                let line = params.selection.map(|r| r.start.line as usize);
                let (ws, owner) = (ws.clone(), owner.clone());
                // Deferred: the editor asking is still being updated, and opening may update it again.
                window.defer(cx, move |window, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.open_file(&owner, target, line, window, cx));
                });
                true
            }));
        });
        self.pull_diagnostics(id, path, cx);
    }

    /// The tab's text changed: send it to the server once typing pauses (`immediately` for reloads).
    pub(super) fn sync_with_server(&mut self, id: &Id, path: &Path, immediately: bool, cx: &mut Context<Self>) {
        let Some(tab) = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
        if tab.server.is_none() {
            return;
        }
        tab.version += 1;
        let version = tab.version;
        let (id, path) = (id.clone(), path.to_path_buf());
        cx.spawn(async move |this, cx| {
            if !immediately {
                cx.background_executor().timer(SYNC_DELAY).await;
            }
            let _ = this.update(cx, |ws, cx| {
                let Some(tab) = ws.panes.get(&id).and_then(|p| p.tabs.iter().find(|t| t.path == path)) else { return };
                // A later edit has its own pending send.
                let (Some(server), Some(uri), true) = (&tab.server, lsp::uri(&path), tab.version == version) else { return };
                server.notify::<DidChangeTextDocument>(lsp_types::DidChangeTextDocumentParams {
                    text_document: lsp_types::VersionedTextDocumentIdentifier::new(uri, version),
                    content_changes: vec![lsp_types::TextDocumentContentChangeEvent { range: None, range_length: None, text: tab.buffer.read(cx).value().to_string() }],
                });
                ws.pull_diagnostics(&id, &path, cx);
            });
        })
        .detach();
    }

    /// Ask a server that doesn't send diagnostics for the tab's (see `lsp::Kind::pull_diagnostics`).
    fn pull_diagnostics(&self, id: &Id, path: &Path, cx: &mut Context<Self>) {
        let Some(tab) = self.panes.get(id).and_then(|p| p.tabs.iter().find(|t| t.path == path)) else { return };
        let (Some(server), Some(uri)) = (tab.server.as_ref().filter(|s| s.pulls_diagnostics()), lsp::uri(path)) else { return };
        let (reply, version) = (server.diagnostics(&uri), tab.version);
        let (id, path) = (id.clone(), path.to_path_buf());
        cx.spawn(async move |this, cx| {
            let Ok(Some(diagnostics)) = reply.await else { return };
            let _ = this.update(cx, |ws, cx| {
                // A later edit asks again; this answer is about older text.
                let Some(tab) = ws.panes.get(&id).and_then(|p| p.tabs.iter().find(|t| t.path == path)).filter(|t| t.version == version) else { return };
                show_diagnostics(tab, diagnostics, cx);
            });
        })
        .detach();
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
}

fn show_diagnostics(tab: &Tab, mut diagnostics: Vec<lsp_types::Diagnostic>, cx: &mut App) {
    let utf16 = tab.server.as_ref().is_some_and(|s| s.counts_utf16());
    tab.buffer.update(cx, |b, cx| {
        let text = b.text().clone();
        if utf16 {
            for d in &mut diagnostics {
                d.range = range_in_chars(&text, d.range);
            }
        }
        if let Some(set) = b.diagnostics_mut() {
            set.reset(&text);
            set.extend(diagnostics);
        }
        cx.notify();
    });
}

/// One open file as the editor's hover and definition provider.
struct Document {
    server: Rc<Server>,
    uri: Uri,
}

impl Document {
    fn at(&self, text: &Rope, offset: usize) -> TextDocumentPositionParams {
        let mut at = text.offset_to_position(offset);
        if self.server.counts_utf16() {
            at.character = lsp::chars_to_utf16(&line_of(text, at.line), at.character);
        }
        TextDocumentPositionParams::new(TextDocumentIdentifier::new(self.uri.clone()), at)
    }
}

fn line_of(text: &Rope, line: u32) -> String {
    text.slice_line(line as usize).to_string()
}

/// A server's UTF-16 range in `text` with its columns in characters, as the editor counts them.
fn range_in_chars(text: &Rope, range: lsp_types::Range) -> lsp_types::Range {
    let at = |p: lsp_types::Position| lsp_types::Position::new(p.line, lsp::utf16_to_chars(&line_of(text, p.line), p.character));
    lsp_types::Range::new(at(range.start), at(range.end))
}

impl HoverProvider for Document {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> gpui::Task<anyhow::Result<Option<lsp_types::Hover>>> {
        let reply = self.server.request::<HoverRequest>(lsp_types::HoverParams { text_document_position_params: self.at(text, offset), work_done_progress_params: Default::default() });
        cx.background_spawn(async move { reply.await.map_err(anyhow::Error::msg) })
    }
}

impl DefinitionProvider for Document {
    fn definitions(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> gpui::Task<anyhow::Result<Vec<LocationLink>>> {
        let reply = self.server.request::<GotoDefinition>(lsp_types::GotoDefinitionParams {
            text_document_position_params: self.at(text, offset),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        // A jump within this file lands by column; one into another file opens it at its line only.
        let (utf16, uri, text) = (self.server.counts_utf16(), self.uri.clone(), text.clone());
        cx.background_spawn(async move {
            let mut links = links(reply.await.map_err(anyhow::Error::msg)?);
            for link in links.iter_mut().filter(|l| utf16 && l.target_uri == uri) {
                link.target_range = range_in_chars(&text, link.target_range);
                link.target_selection_range = range_in_chars(&text, link.target_selection_range);
            }
            Ok(links)
        })
    }
}

/// Servers answer with locations or links; the editor takes links.
fn links(reply: Option<GotoDefinitionResponse>) -> Vec<LocationLink> {
    let link = |l: lsp_types::Location| LocationLink { origin_selection_range: None, target_uri: l.uri, target_range: l.range, target_selection_range: l.range };
    match reply {
        None => vec![],
        Some(GotoDefinitionResponse::Scalar(l)) => vec![link(l)],
        Some(GotoDefinitionResponse::Array(ls)) => ls.into_iter().map(link).collect(),
        Some(GotoDefinitionResponse::Link(ls)) => ls,
    }
}
