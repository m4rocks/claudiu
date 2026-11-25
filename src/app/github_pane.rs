//! The GitHub pane from the workspace: opening it for a project or an attached item, syncing it, and its events.

use super::*;

impl Workspace {
    pub fn gh_pane(&self, id: &str) -> Option<&GhPane> {
        self.gh.iter().find(|p| p.id == id)
    }

    /// Show the project's GitHub view in the right sidebar and move the keyboard into it.
    pub fn open_github(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.github_pane(cwd, window, cx);
        self.show_github(id, window, cx);
    }

    fn show_github(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        self.right_gh = Some(id.clone());
        self.store.data.layout.right_hidden = false;
        self.store.mark_dirty();
        if let Some(p) = self.gh_pane(&id) {
            let handle = p.view.read(cx).focus.clone();
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// An attached issue or pull request was clicked: show it in the GitHub view of the shown session's project, or of
    /// the repository it names.
    pub fn open_attached(&mut self, repo: Option<&str>, pull: bool, number: u32, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd();
        // Claude may name the project's own repository; its own pane shows that, with every action.
        let project_repo = {
            let id = self.github_pane(cwd.clone(), window, cx);
            self.gh_pane(&id).and_then(|p| p.view.read(cx).repo.as_ref()).map(|r| r.full())
        };
        let repo = repo.filter(|full| !project_repo.is_some_and(|own| own.eq_ignore_ascii_case(full)));
        match repo {
            Some(full) => {
                let id = self.named_github_pane(cwd, full, window, cx);
                self.show_github(id, window, cx);
            }
            None => self.open_github(cwd, window, cx),
        }
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
        match self.gh.iter().find(|p| p.view.read(cx).named.is_none() && platform::path_key(&p.cwd) == key) {
            Some(p) => p.id.clone(),
            None => {
                let name = project
                    .map(|p| p.name)
                    .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "GitHub".into());
                let id = new_id();
                let view = cx.new(|cx| GithubView::new(root.clone(), name.clone(), None, crate::github::cli::locate().map(|exe| crate::github::cli::Gh::new(exe, root.clone())), window, cx));
                self.push_github_pane(id.clone(), root, name, view, window, cx);
                id
            }
        }
    }

    /// The GitHub view of the repository `full` (`owner/name`), which has no project here, created on first use.
    /// `cwd` is only where its `gh` runs.
    fn named_github_pane(&mut self, cwd: PathBuf, full: &str, window: &mut Window, cx: &mut Context<Self>) -> Id {
        if let Some(p) = self.gh.iter().find(|p| p.view.read(cx).named.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(full))) {
            return p.id.clone();
        }
        let id = new_id();
        let gh = crate::github::cli::locate().map(|exe| crate::github::cli::Gh::new(exe, cwd.clone()));
        let view = cx.new(|cx| GithubView::new(cwd.clone(), full.to_string(), Some(full.to_string()), gh, window, cx));
        self.push_github_pane(id.clone(), cwd, full.to_string(), view, window, cx);
        id
    }

    fn push_github_pane(&mut self, id: Id, cwd: PathBuf, project: String, view: Entity<GithubView>, window: &mut Window, cx: &mut Context<Self>) {
        let sub_id = id.clone();
        let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, _view, ev: &GhEvent, window, cx| ws.on_gh_event(&sub_id, ev, window, cx));
        // Returning to the window refreshes the pane, but only the one that is on screen.
        let (shown, watched) = (id.clone(), view.clone());
        let active = cx.observe_window_activation(window, move |ws: &mut Workspace, window, cx| {
            if window.is_window_active() && ws.right_gh.as_ref() == Some(&shown) && !ws.store.data.layout.right_hidden {
                watched.update(cx, |v, cx| v.refresh_on_return(window, cx));
            }
        });
        self.gh.push(GhPane { id, cwd, project, view, _sub: sub, _active: active });
    }

    fn on_gh_event(&mut self, pane: &str, ev: &GhEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.gh_pane(pane).map(|p| p.cwd.clone()) else { return };
        match ev {
            GhEvent::Toast(text) | GhEvent::Error(text) => self.toast(text.clone(), cx),
            GhEvent::AskClaude(prompt) => match self.running_agent_in(&cwd, cx) {
                Some(id) => self.send_to_agent(&id, prompt, window, cx),
                None => {
                    let chosen = self.home_selected_model();
                    let options = (!chosen.id.is_empty()).then_some(("model", chosen.id)).into_iter().collect();
                    self.new_session_with(chosen.harness, cwd, Some(prompt.clone()), options, Starter::User, None, window, cx);
                }
            },
            GhEvent::Checkout(number) => {
                let Some(exe) = crate::github::cli::locate() else { return };
                let number = *number;
                self.run_git(
                    GitOp::Branch,
                    move || crate::github::cli::Gh::new(exe, cwd).checkout(number).map_err(|e| e.msg.into()),
                    cx,
                );
            }
            GhEvent::SignIn => self.sign_in_to_github(window, cx),
        }
    }

    /// `gh auth login` in a terminal tab.
    pub(super) fn sign_in_to_github(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            agent: None,
        };
        self.start(record, spec, true, window, cx);
    }
}
