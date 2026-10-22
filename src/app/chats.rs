//! Chat panes in the workspace: starting one, reopening a past one, and keeping the history list current.
//! A chat is a pane like the GitHub one (it has no terminal and no `state.json` record) with its own `chats.json`.

use super::*;

const TITLE_SYSTEM: &str = "You name conversations. Reply with the title only: two to six words, no quotes, no trailing punctuation, in the language the conversation is mostly in. Say what the conversation is about, not how it started.";

/// A chat whose agent is running. Closing the pane drops the view, which ends the agent; the record stays.
pub struct ChatPane {
    pub id: Id,
    pub cwd: PathBuf,
    pub view: Entity<ChatView>,
    /// The Claudiu account the agent runs under; limits it reports are filed there.
    pub account: Id,
    /// Context tokens used and the window size, for the sidebar's CONTEXT meter.
    pub context: Option<(u64, u64)>,
    /// What the chat's mod reported (see `usage.rs`); `None` until it has.
    pub auto_compact: Option<bool>,
    pub compact_at: Option<u64>,
    pub advisor: Option<String>,
    /// When the last turn ended: the prompt cache is warm for a while after it.
    pub turn_at: Option<i64>,
    pub _sub: Subscription,
}

impl Workspace {
    pub fn chat_pane(&self, id: &str) -> Option<&ChatPane> {
        self.chats.iter().find(|p| p.id == id)
    }

    /// Title of a chat, for the top bar.
    pub fn chat_title(&self, id: &str) -> Option<String> {
        self.chat_pane(id).and_then(|p| self.chat_store.get(&p.id)).map(|r| r.title.clone())
    }

    /// Chats belong to no account: Claude's history is shared between them, so a chat always runs under the active
    /// one and switching accounts only changes whose usage it spends.
    fn chat_launch(&self) -> Option<(Launch, Id)> {
        let claude = self.claude_exe.clone()?;
        let acct = self.store.active_account()?;
        let mut env = Self::account_env(acct);
        env.push((usage::ACCOUNT_ENV.into(), acct.id.clone()));
        Some((Launch { claude, env }, acct.id.clone()))
    }

    /// Start the chat adapter in the background so a chat opens ready (it is shared by all chats).
    pub fn warm_chats(&self, cx: &mut Context<Self>) {
        if let Some((launch, _)) = self.chat_launch() {
            Self::warm(launch, cx);
        }
    }

    fn warm(launch: Launch, cx: &mut Context<Self>) {
        cx.background_executor().spawn(async move { crate::chat::acp::warm(&launch.claude, launch.env) }).detach();
    }

    /// Move every open chat to the newly active account once it is idle (same conversation, new login).
    pub(super) fn relaunch_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((launch, account)) = self.chat_launch() else { return };
        Self::warm(launch.clone(), cx);
        for pane in &mut self.chats {
            pane.account = account.clone();
            pane.view.update(cx, |v, cx| v.relaunch(Some(launch.clone()), window, cx));
        }
    }

    /// Projects a chat can be tied to, as (name, folder).
    fn project_choices(&self) -> Vec<(String, PathBuf)> {
        self.store.data.projects.iter().map(|p| (p.name.clone(), p.path.clone())).collect()
    }

    /// A new chat. It belongs to no project unless `cwd` names one; `prompt` is sent as its first message.
    pub fn new_chat(&mut self, cwd: Option<PathBuf>, prompt: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Id {
        let cwd = cwd.unwrap_or_else(scratch_dir);
        let rec = ChatRecord {
            id: new_id(),
            session_id: None,
            cwd,
            title: DEFAULT_TITLE.into(),
            created_at: now(),
            last_active: now(),
            removed: false,
            draft: String::new(),
            links: vec![],
            attached: vec![],
        };
        let id = rec.id.clone();
        self.chat_store.add(rec);
        self.open_chat(&id, window, cx);
        if let (Some(prompt), Some(pane)) = (prompt, self.chat_pane(&id)) {
            pane.view.update(cx, |v, _| v.send_when_ready(prompt));
        }
        id
    }

    /// Show a chat, starting its agent and loading the conversation first if it is not running.
    pub fn open_chat(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_pane(id).is_none() {
            let Some(rec) = self.chat_store.get(id).cloned() else { return };
            let (launch, account) = match self.chat_launch() {
                Some((l, a)) => (Some(l), a),
                None => (None, MAIN_ACCOUNT.to_string()),
            };
            let cwd = rec.cwd.clone();
            if !is_tied(&cwd) {
                let _ = std::fs::create_dir_all(&cwd);
            }
            // A chat nothing was said in has no conversation to load (its empty session is not one Claude can resume).
            let resume = rec.session_id.clone().filter(|_| rec.title != DEFAULT_TITLE);
            let view = cx.new(|cx| ChatView::new(rec.id.clone(), rec.cwd.clone(), launch, resume, window, cx));
            if !rec.draft.is_empty() {
                view.update(cx, |v, cx| v.set_draft(&rec.draft, window, cx));
            }
            let projects = self.project_choices();
            view.update(cx, |v, _| v.set_projects(projects));
            let sub_id = rec.id.clone();
            let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, _view, ev: &ChatEvent, window, cx| ws.on_chat_event(&sub_id, ev, window, cx));
            self.chats.push(ChatPane { id: rec.id, cwd, view, account, context: None, auto_compact: None, compact_at: None, advisor: None, turn_at: None, _sub: sub });
        }
        self.show_session(id, window, cx);
    }

    fn on_chat_event(&mut self, id: &str, ev: &ChatEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            // Typing is saved on the next tick, not on every key.
            ChatEvent::Draft(text) => {
                self.chat_store.update(id, |r| r.draft = text.clone());
                return;
            }
            ChatEvent::Toast { text, error } => self.toast(text.clone(), *error, cx),
            ChatEvent::Session(sid) => self.chat_store.update(id, |r| r.session_id = Some(sid.clone())),
            ChatEvent::Title(t) => self.chat_store.update(id, |r| r.title = t.clone()),
            ChatEvent::Sent(text) => self.chat_store.update(id, |r| {
                r.last_active = now();
                r.draft.clear();
                if r.title == DEFAULT_TITLE {
                    r.title = title_from(text);
                }
            }),
            ChatEvent::Cwd(cwd) => {
                self.chat_store.update(id, |r| {
                    r.cwd = cwd.clone();
                    r.session_id = None;
                });
                if let Some(p) = self.chats.iter_mut().find(|p| p.id == id) {
                    p.cwd = cwd.clone();
                }
            }
            ChatEvent::Turn => {
                if let Some(p) = self.chats.iter_mut().find(|p| p.id == id) {
                    p.turn_at = Some(now());
                }
            }
            ChatEvent::Context(used, size) => {
                if let Some(p) = self.chats.iter_mut().find(|p| p.id == id) {
                    p.context = Some((*used, *size));
                }
            }
            ChatEvent::Limits { five, seven } => {
                if let Some(account) = self.chat_pane(id).map(|p| p.account.clone()) {
                    self.merge_account(&account, five.clone(), seven.clone());
                }
            }
        }
        self.chat_store.save_if_dirty();
        cx.notify();
    }

    /// A chat that never got a message has nothing worth keeping once it is closed.
    pub(super) fn forget_if_unused(&mut self, id: &str) {
        if self.chat_store.get(id).is_some_and(|r| r.is_blank()) {
            self.chat_store.remove(id);
        }
        self.chat_store.save_if_dirty();
    }

    /// Have Haiku write a new title from what was said, and use it as the name of the chat or Claude Code session
    /// (a session's name also reaches Claude Code itself through `sync_claude_names`).
    pub(super) fn regen_title(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(claude) = self.claude_exe.clone() else { return self.toast("Could not find the `claude` CLI.", true, cx) };
        let is_chat = self.chat_store.get(id).is_some();
        // What was said: a chat's messages as they are on screen (or last saved), a session's transcript file.
        let (said, transcript) = if is_chat {
            let items = self.chat_pane(id).map(|p| p.view.read(cx).transcript(40));
            (items.unwrap_or_else(|| crate::chat::model::transcript(&crate::chat::store::load_cache(id), 40)), None)
        } else {
            (String::new(), self.store.session(id).and_then(|s| s.claude.as_ref()?.transcript.clone()))
        };
        if said.is_empty() && transcript.is_none() {
            return self.toast("Nothing to title yet.", false, cx);
        }
        self.toast("Writing a new title…", false, cx);
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let said = transcript.map_or(said, |p| crate::claude::conversation_text(&p));
                    if said.trim().is_empty() {
                        return Err("Nothing to title yet.".to_string());
                    }
                    let prompt = format!("Conversation:

{said}

Write the title.");
                    let raw = crate::commit::generate(&claude, TITLE_SYSTEM, &prompt)?;
                    let title: String = raw.lines().next().unwrap_or_default().trim().trim_matches(['"', '\'', '`', '.']).chars().take(60).collect();
                    if title.is_empty() { Err("claude returned no title".to_string()) } else { Ok(title) }
                })
                .await;
            let _ = this.update(cx, |ws, cx| match result {
                Ok(title) => {
                    if ws.chat_store.get(&id).is_some() {
                        ws.chat_store.update(&id, |r| r.title = title.clone());
                        ws.chat_store.save_if_dirty();
                    } else if let Some(rec) = ws.store.session_mut(&id) {
                        rec.custom_title = Some(title.clone());
                    }
                    ws.toast(format!("Renamed to “{title}”"), false, cx);
                }
                Err(e) => ws.toast(e, true, cx),
            });
        })
        .detach();
    }

    /// Remove a chat from the list. Claude Code's own transcript is not touched.
    pub fn remove_chat(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_pane(id).is_some() {
            self.close_live(id, window, cx);
        }
        self.chat_store.hide(id);
        crate::chat::store::drop_cache(id);
        self.chat_store.save_if_dirty();
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        self.toast("Removed from the list. The Claude transcript was not touched.", false, cx);
    }

    pub fn chat_menu(&self, id: &str) -> Vec<MenuItem> {
        let Some(rec) = self.chat_store.get(id) else { return vec![] };
        let live = self.chat_pane(id).is_some();
        let item = |label: &str, act: Act, danger: bool, sep: bool| MenuItem { label: label.into(), hint: None, act, danger, disabled: false, separator_before: sep };
        let mut items = vec![item(if live { "Switch to chat" } else { "Open chat" }, Act::OpenChat(id.into()), false, false)];
        items.push(item("Regenerate title", Act::RegenTitle(id.into()), false, false));
        if let Some(sid) = &rec.session_id {
            items.push(item("Copy Claude session ID", Act::CopyText(sid.clone()), false, true));
        }
        if is_tied(&rec.cwd) {
            items.push(item("Copy folder path", Act::CopyText(rec.cwd.to_string_lossy().into_owned()), false, rec.session_id.is_none()));
            items.push(item(if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in Explorer" }, Act::Reveal(rec.cwd.clone()), false, false));
        }
        if live {
            items.push(item("End chat", Act::CloseLive(id.into()), true, true));
        }
        items.push(item("Remove from list", Act::RemoveChat(id.into()), true, !live));
        items
    }
}
