//! Alt accounts: their config folders for a launch, refreshing their usage and login, and signing one in.

use super::*;

impl Workspace {
    /// Environment for an alt account (empty for the main one), after linking anything new from the main folder into it.
    pub(super) fn account_env(acct: &Account) -> Vec<(String, String)> {
        let Some(dir) = acct.dir.as_ref() else { return vec![] };
        if let Some(main) = claude::claude_home() {
            let _ = std::fs::create_dir_all(dir);
            let _ = accounts::sync_shared(&main, dir);
        }
        accounts::env(dir)
    }

    /// Read each account's email and plan through `claude auth status`, off the UI thread.
    pub fn refresh_accounts(&mut self, cx: &mut Context<Self>) {
        let Some(exe) = self.claude_exe.clone() else { return };
        let jobs: Vec<(Id, Option<PathBuf>)> = self.store.data.accounts.iter().map(|a| (a.id.clone(), a.dir.clone())).collect();
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move { jobs.into_iter().map(|(id, dir)| (id, accounts::status(&exe, dir.as_deref()))).collect::<Vec<_>>() })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let mut signed_in = vec![];
                for (id, status) in out {
                    // No answer (CLI failed to run) keeps what we knew; an answer of "logged out" clears it.
                    let Some(s) = status else { continue };
                    let Some(a) = ws.store.data.accounts.iter_mut().find(|a| a.id == id) else { continue };
                    let (email, plan) = if s.logged_in { (s.email, s.plan) } else { (None, None) };
                    if a.email != email || a.plan != plan {
                        if let Some(e) = email.clone().filter(|_| !a.is_main() && a.email != email) {
                            signed_in.push((a.id.clone(), e));
                        }
                        (a.email, a.plan) = (email, plan);
                        ws.store.mark_dirty();
                    }
                }
                // Warn once, when an alt has just signed in to the main account's login.
                let main = ws.store.account(MAIN_ACCOUNT).and_then(|a| a.email.clone());
                if let Some((id, _)) = signed_in.into_iter().find(|(_, e)| Some(e) == main.as_ref()) {
                    // The login page approves whichever claude.ai account the browser is already signed in to.
                    let text = "This alt signed in to your main account. Sign out of claude.ai in your browser (or use a private window), then sign in again.";
                    ws.show_toast(text.into(), true, Some(("Sign in again".into(), Act::SignIn(id))), cx);
                }
                ws.store.save_if_dirty();
                cx.notify();
            });
        })
        .detach();
    }

    /// `claude auth login` for an alt account in a terminal tab; account details refresh when it ends.
    pub(super) fn sign_in(&mut self, account: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI.", true, cx);
            return;
        };
        let Some(env) = self.store.account(account).map(Self::account_env).filter(|e| !e.is_empty()) else { return };
        let cwd = platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let (program, args) = platform::through_login_shell(exe.to_string_lossy().into_owned(), vec!["auth".into(), "login".into()]);
        let spec = SpawnSpec { program, args, cwd: Some(cwd.clone()), env };
        let record = SessionRecord {
            id: new_id(),
            kind: SessionKind::Shell,
            title: "Sign in to account".into(),
            custom_title: None,
            auto_title: None,
            project_id: None,
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            claude: None,
        };
        self.logins.insert(record.id.clone());
        self.start(record, spec, true, window, cx);
    }
}
