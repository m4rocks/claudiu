//! The GitHub pane from the workspace: opening it for a project or an attached item, syncing it, and its events.

use super::*;

impl Workspace {
    pub fn gh_pane(&self, id: &str) -> Option<&GhPane> {
        self.gh.iter().find(|p| p.id == id)
    }

    /// Show the project's GitHub view in the right sidebar and move the keyboard into it.
    pub fn open_github(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.github_pane(cwd, window, cx);
        self.right_gh = Some(id.clone());
        self.store.data.layout.right_hidden = false;
        self.store.mark_dirty();
        if let Some(p) = self.gh_pane(&id) {
            let handle = p.view.read(cx).focus.clone();
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// An attached issue or pull request was clicked: show it in the GitHub view of the shown session's project.
    pub fn open_attached(&mut self, pull: bool, number: u32, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd();
        self.open_github(cwd, window, cx);
        let kind = if pull { crate::github::model::Kind::Pull } else { crate::github::model::Kind::Issue };
        if let Some(view) = self.right_gh.as_ref().and_then(|id| self.gh_pane(id)).map(|p| p.view.clone()) {
            view.update(cx, |v, cx| v.open_item(kind, number, window, cx));
        }
    }

    /// While the right sidebar shows GitHub, it follows the shown session to that session's project (no focus change).
    pub(super) fn sync_github(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only the panel on screen follows; other projects' panes are made when the panel opens.
        if self.right_gh.is_none() {
            return;
        }
        let Some(id) = self.active.clone() else { return };
        if let Some(cwd) = self.store.session(&id).map(|s| s.cwd.clone()) {
            self.right_gh = Some(self.github_pane(cwd, window, cx));
        }
    }

    /// The project's GitHub view (one per project), created on first use.
    pub(super) fn github_pane(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Id {
        let project = self.store.project_for_cwd(&cwd).and_then(|id| self.store.project(&id)).cloned();
        let root = project.as_ref().map(|p| p.path.clone()).unwrap_or(cwd);
        let key = platform::path_key(&root);
        match self.gh.iter().find(|p| platform::path_key(&p.cwd) == key) {
            Some(p) => p.id.clone(),
            None => {
                let name = project
                    .map(|p| p.name)
                    .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "GitHub".into());
                let id = new_id();
                let view = cx.new(|cx| GithubView::new(root.clone(), name.clone(), crate::github::cli::locate().map(|exe| crate::github::cli::Gh::new(exe, root.clone())), window, cx));
                let sub_id = id.clone();
                let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, _view, ev: &GhEvent, window, cx| ws.on_gh_event(&sub_id, ev, window, cx));
                // Returning to the window refreshes the pane, but only the one that is on screen.
                let (shown, watched) = (id.clone(), view.clone());
                let active = cx.observe_window_activation(window, move |ws: &mut Workspace, window, cx| {
                    if window.is_window_active() && ws.right_gh.as_ref() == Some(&shown) && !ws.store.data.layout.right_hidden {
                        watched.update(cx, |v, cx| v.refresh_on_return(window, cx));
                    }
                });
                self.gh.push(GhPane { id: id.clone(), cwd: root, project: name, view, _sub: sub, _active: active });
                id
            }
        }
    }

    fn on_gh_event(&mut self, pane: &str, ev: &GhEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.gh_pane(pane).map(|p| p.cwd.clone()) else { return };
        match ev {
            GhEvent::Toast { text, error } => self.toast(text.clone(), *error, cx),
            GhEvent::AskClaude(prompt) => self.new_claude_with(cwd, vec![prompt.clone()], window, cx),
            GhEvent::Checkout(number) => {
                let Some(exe) = crate::github::cli::locate() else { return };
                let number = *number;
                self.run_git(
                    GitOp::Branch,
                    move || crate::github::cli::Gh::new(exe, cwd).checkout(number).map_err(|e| e.msg.into()),
                    cx,
                );
            }
            GhEvent::SignIn => {
                let Some(exe) = crate::github::cli::locate() else { return };
                let home = platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
                let spec = SpawnSpec { program: exe.to_string_lossy().into_owned(), args: vec!["auth".into(), "login".into()], cwd: Some(home.clone()), ..Default::default() };
                let record = SessionRecord {
                    id: new_id(),
                    kind: SessionKind::Shell,
                    title: "GitHub sign in".into(),
                    custom_title: None,
                    auto_title: None,
                    project_id: None,
                    cwd: home,
                    created_at: now(),
                    last_active: now(),
                    exit_code: None,
                    hidden: false,
                    claude: None,
                };
                self.start(record, spec, true, window, cx);
            }
        }
    }
}
