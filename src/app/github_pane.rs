//! The GitHub pane from the workspace: opening it for a project or an attached item, syncing it, and its events.

use super::*;

impl Workspace {
    pub fn gh_pane(&self, id: &str) -> Option<&GhPane> {
        self.gh.iter().find(|p| p.id == id)
    }

    pub fn gh_view_mut(&mut self, id: &str) -> Option<&mut GithubView> {
        self.gh.iter_mut().find(|p| p.id == id).map(|p| &mut p.view)
    }

    /// The text field `which` picks in pane `id`. A pane that is gone hands out a scratch field (the search's), which
    /// can only happen between a pane closing and the next frame.
    pub fn gh_field(&mut self, id: &str, which: fn(&mut GithubView) -> &mut rune::TextEdit) -> &mut rune::TextEdit {
        match self.gh.iter().position(|p| p.id == id) {
            Some(i) => which(&mut self.gh[i].view),
            None => &mut self.search,
        }
    }

    /// Run what the GitHub panes asked for since the last handler, and hand their `Tx` work to the frame.
    pub(super) fn drain_github(&mut self) {
        let mut events = vec![];
        for p in &mut self.gh {
            events.extend(p.view.events.drain(..).map(|e| (p.id.clone(), e)));
            for f in p.view.fx.drain(..) {
                self.rt.with_tx(f);
            }
        }
        for (pane, ev) in events {
            self.on_gh_event(&pane, &ev);
        }
    }

    /// Show the project's GitHub view in the right sidebar and move the keyboard into it.
    pub fn open_github(&mut self, cwd: PathBuf) {
        let id = self.github_pane(cwd);
        self.right_gh = Some(id.clone());
        self.store.data.layout.right_hidden = false;
        self.store.mark_dirty();
        // The pane takes keys through its own fields; the terminal lets go of them.
        self.keys_to = None;
        self.blur = true;
    }

    /// An attached issue or pull request was clicked: show it in the GitHub view of the shown session's project.
    pub fn open_attached(&mut self, pull: bool, number: u32) {
        let cwd = self.active_cwd();
        self.open_github(cwd);
        let kind = if pull { crate::github::model::Kind::Pull } else { crate::github::model::Kind::Issue };
        if let Some(id) = self.right_gh.clone()
            && let Some(p) = self.gh.iter_mut().find(|p| p.id == id)
        {
            p.view.open_item(kind, number);
        }
    }

    /// While the right sidebar shows GitHub, it follows the shown session to that session's project (no focus change).
    pub(super) fn sync_github(&mut self) {
        // Only the panel on screen follows; other projects' panes are made when the panel opens.
        if self.right_gh.is_none() {
            return;
        }
        let Some(id) = self.active.clone() else { return };
        if let Some(cwd) = self.store.session(&id).map(|s| s.cwd.clone()) {
            self.right_gh = Some(self.github_pane(cwd));
        }
    }

    /// The project's GitHub view (one per project), created on first use.
    pub(super) fn github_pane(&mut self, cwd: PathBuf) -> Id {
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
                let gh = crate::github::cli::locate().map(|exe| crate::github::cli::Gh::new(exe, root.clone()));
                let view = GithubView::new(id.clone(), self.rt.mailbox(), root.clone(), name.clone(), gh);
                self.gh.push(GhPane { id: id.clone(), cwd: root, project: name, view });
                id
            }
        }
    }

    /// Run what a GitHub pane asked for.
    pub(crate) fn on_gh_event(&mut self, pane: &str, ev: &GhEvent) {
        let Some(cwd) = self.gh_pane(pane).map(|p| p.cwd.clone()) else { return };
        match ev {
            GhEvent::Toast { text, error } => self.toast(text.clone(), *error),
            GhEvent::AskClaude(prompt) => match self.running_claude_in(&cwd) {
                Some(id) => self.send_to_claude(&id, prompt),
                None => self.new_claude_with(cwd, vec![prompt.clone()], Starter::User),
            },
            GhEvent::Checkout(number) => {
                let Some(exe) = crate::github::cli::locate() else { return };
                let number = *number;
                self.run_git(
                    GitOp::Branch,
                    move || crate::github::cli::Gh::new(exe, cwd).checkout(number).map_err(|e| e.msg.into()),
                );
            }
            GhEvent::SignIn => self.sign_in_to_github(),
        }
    }

    /// `gh auth login` in a terminal tab.
    pub(super) fn sign_in_to_github(&mut self) {
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
        self.start_session(record, spec, true);
    }
}
