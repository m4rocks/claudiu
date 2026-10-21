//! Claudiu's own persisted state (projects, session history, cached usage, layout).
//! Independent of GPUI. Claude Code's data is only ever *read* (see `claude.rs`) and merged in here.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::GitInfo;
use crate::platform::{is_same_or_under, path_key};

pub type Id = String;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn new_id() -> Id {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    Claude,
    Shell,
}

/// Whether Claude Code's own record of the session was found on the last scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalState {
    /// Created by Claudiu, not yet seen on disk (or never scanned).
    #[default]
    Unknown,
    Present,
    /// Previously seen, absent from the last successful scan. Kept in history, flagged stale.
    Missing,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeMeta {
    pub session_id: String,
    pub model: Option<String>,
    pub context_tokens: Option<u64>,
    /// Exact context usage (0..=1) and window size as reported by Claude Code's status line, when seen.
    #[serde(default)]
    pub context_fraction: Option<f32>,
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Prompt-cache lifetime in seconds (300 or 3600), read from the last cache write in the transcript.
    #[serde(default)]
    pub cache_ttl: Option<u64>,
    /// Unix time of the last request that read or wrote the prompt cache (the moment its TTL restarted).
    #[serde(default)]
    pub cache_at: Option<i64>,
    pub transcript: Option<PathBuf>,
    #[serde(default)]
    pub external: ExternalState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: Id,
    pub kind: SessionKind,
    /// Title as known from Claude Code (or a default). Never edited by the user.
    pub title: String,
    /// Claudiu-only display label; wins over `title` when set.
    #[serde(default)]
    pub custom_title: Option<String>,
    /// Tab title Claude chose itself through Claudiu's MCP tool (see mcp.rs). Beats `title`, loses to `custom_title`.
    #[serde(default)]
    pub auto_title: Option<String>,
    pub cwd: PathBuf,
    pub project_id: Option<Id>,
    pub created_at: i64,
    pub last_active: i64,
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Removed from Claudiu's index. The underlying Claude transcript is untouched.
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub claude: Option<ClaudeMeta>,
}

impl SessionRecord {
    pub fn display_title(&self) -> &str {
        self.chosen_title().unwrap_or(&self.title)
    }

    /// The name given in Claudiu (manual rename, else Claude's own MCP title); what Claude Code is told to use.
    pub fn chosen_title(&self) -> Option<&str> {
        [&self.custom_title, &self.auto_title].into_iter().flatten().map(|t| t.trim()).find(|t| !t.is_empty())
    }

    pub fn claude_id(&self) -> Option<&str> {
        self.claude.as_ref().map(|c| c.session_id.as_str())
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub git: Option<GitInfo>,
    #[serde(default = "yes")]
    pub collapsed: bool,
    pub added_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimit {
    /// 0.0..=1.0 when known.
    pub utilization: Option<f32>,
    /// Unix seconds.
    pub resets_at: Option<i64>,
    /// When Claudiu observed this value.
    pub seen_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountUsage {
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
}

impl AccountUsage {
    /// The higher of the 5-hour and 7-day use that still applies at `now`; a window past its reset counts as 0.
    pub fn peak(&self, now: i64) -> f32 {
        [&self.five_hour, &self.seven_day]
            .into_iter()
            .flatten()
            .filter(|l| l.resets_at.is_none_or(|r| r > now))
            .filter_map(|l| l.utilization)
            .fold(0.0, f32::max)
    }
}

impl Account {
    pub fn is_main(&self) -> bool {
        self.dir.is_none()
    }

    /// Email when known, else which kind of account it is.
    pub fn name(&self) -> String {
        match (&self.email, self.is_main()) {
            (Some(e), _) => e.clone(),
            (None, true) => "Main account".into(),
            (None, false) => "Not signed in".into(),
        }
    }
}

/// Id of the main account: the user's normal Claude Code (`~/.claude`).
pub const MAIN_ACCOUNT: &str = "main";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: Id,
    /// Alt account's Claude Code config folder (see accounts.rs); `None` is the main account.
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// From `claude auth status`; `None` until read, or when signed out.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub usage: AccountUsage,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub selected_session: Option<Id>,
    pub sidebar_visible: bool,
    pub other_collapsed: bool,
    /// The sidebar showed the Chat tab (not Code) when Claudiu last ran.
    #[serde(default)]
    pub chat_tab: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Explicit path to the `claude` executable; discovery is used when empty.
    pub claude_path: Option<PathBuf>,
    /// Program for new terminal sessions (a path or a name on PATH); the platform default shell when empty.
    #[serde(default)]
    pub shell: Option<String>,
    /// Update feed base URL. Empty/None disables update checks entirely.
    pub update_url: Option<String>,
    /// Editor the "Open in" button uses; the last one picked.
    #[serde(default)]
    pub editor: Option<crate::platform::Editor>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Data {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<SessionRecord>,
    /// Usage from before accounts existed; moved into the main account on load. Still written (empty after the
    /// move) because older Claudiu versions require the field and would set state.json aside without it.
    #[serde(default)]
    pub account: AccountUsage,
    /// The main account first, then alts. Kept in `accounts.json`, not state.json: an older Claudiu still
    /// running would rewrite state.json without them.
    #[serde(skip)]
    pub accounts: Vec<Account>,
    /// Account new sessions start with; the main one when unset.
    #[serde(skip)]
    pub active_account: Option<Id>,
    pub layout: Layout,
    pub settings: Settings,
}

#[derive(Default, Serialize, Deserialize)]
struct AccountsFile {
    #[serde(default)]
    accounts: Vec<Account>,
    #[serde(default)]
    active: Option<Id>,
}

fn accounts_path(state: &Path) -> PathBuf {
    state.with_file_name("accounts.json")
}

pub struct Store {
    path: PathBuf,
    pub data: Data,
    dirty: bool,
}

impl Store {
    /// Platform app-data location, e.g. `%LOCALAPPDATA%\Claudiu\data\state.json`.
    pub fn default_path() -> PathBuf {
        // Tests (and anyone running a second copy) can keep their state away from the real one.
        if let Some(dir) = std::env::var_os("CLAUDIU_DATA_DIR") {
            return PathBuf::from(dir).join("state.json");
        }
        // Local (non-roaming) data: it holds machine-specific paths.
        directories::ProjectDirs::from("", "", "Claudiu")
            .map(|d| d.data_local_dir().join("state.json"))
            .unwrap_or_else(|| PathBuf::from("claudiu-state.json"))
    }

    /// Load tolerant of missing/corrupt files: a corrupt file is moved aside, never overwritten silently.
    pub fn load(path: PathBuf) -> Self {
        let mut data = Data { version: 1, layout: Layout { sidebar_visible: true, ..Default::default() }, ..Default::default() };
        if let Ok(text) = fs::read_to_string(&path) {
            match serde_json::from_str::<Data>(&text) {
                Ok(d) => data = d,
                Err(_) => {
                    let _ = fs::rename(&path, path.with_extension("json.corrupt"));
                }
            }
        }
        // Terminal (shell) sessions are never kept across runs; Claude sessions stay as resumable history.
        data.sessions.retain(|s| s.kind != SessionKind::Shell);
        if let Ok(text) = fs::read_to_string(accounts_path(&path)) {
            match serde_json::from_str::<AccountsFile>(&text) {
                Ok(f) => (data.accounts, data.active_account) = (f.accounts, f.active),
                Err(_) => {
                    let _ = fs::rename(accounts_path(&path), accounts_path(&path).with_extension("json.corrupt"));
                }
            }
        }
        if !data.accounts.iter().any(|a| a.id == MAIN_ACCOUNT) {
            let usage = std::mem::take(&mut data.account);
            data.accounts.insert(0, Account { id: MAIN_ACCOUNT.into(), usage, ..Default::default() });
        }
        Self { path, data, dirty: false }
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.data.accounts.iter().find(|a| a.id == id)
    }

    /// The account new sessions use; falls back to the main one if the chosen alt was removed.
    pub fn active_account(&self) -> Option<&Account> {
        self.data.active_account.as_deref().and_then(|id| self.account(id)).or_else(|| self.account(MAIN_ACCOUNT))
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn save_if_dirty(&mut self) {
        if self.dirty {
            self.save();
        }
    }

    pub fn save(&mut self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let write = |path: &Path, text: serde_json::Result<String>| {
            let tmp = path.with_extension("json.tmp");
            text.is_ok_and(|t| fs::write(&tmp, t).is_ok() && fs::rename(&tmp, path).is_ok())
        };
        let accounts = AccountsFile { accounts: self.data.accounts.clone(), active: self.data.active_account.clone() };
        let ok = write(&accounts_path(&self.path), serde_json::to_string_pretty(&accounts));
        if write(&self.path, serde_json::to_string_pretty(&self.data)) && ok {
            self.dirty = false;
        }
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.data.projects.iter().find(|p| p.id == id)
    }

    pub fn session(&self, id: &str) -> Option<&SessionRecord> {
        self.data.sessions.iter().find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: &str) -> Option<&mut SessionRecord> {
        self.dirty = true;
        self.data.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Register a folder as a project (idempotent). Re-maps already-known sessions that live under it.
    pub fn add_project(&mut self, path: &Path) -> Id {
        let path = crate::platform::normalize(path);
        if let Some(existing) = self.data.projects.iter().find(|p| path_key(&p.path) == path_key(&path)) {
            return existing.id.clone();
        }
        let git = crate::git::detect(&path);
        // Prefer the repo root as the project path when the chosen folder is inside a repository.
        let root = git.as_ref().map(|g| g.root.clone()).unwrap_or(path);
        if let Some(existing) = self.data.projects.iter().find(|p| path_key(&p.path) == path_key(&root)) {
            return existing.id.clone();
        }
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string_lossy().into_owned());
        let project = Project { id: new_id(), name, path: root, git, collapsed: true, added_at: now() };
        let id = project.id.clone();
        self.data.projects.push(project);
        self.remap_sessions();
        self.dirty = true;
        id
    }

    pub fn remove_project(&mut self, id: &str) {
        self.data.projects.retain(|p| p.id != id);
        for s in self.data.sessions.iter_mut().filter(|s| s.project_id.as_deref() == Some(id)) {
            s.project_id = None;
        }
        self.dirty = true;
    }

    pub fn refresh_git(&mut self) {
        for p in &mut self.data.projects {
            p.git = crate::git::detect(&p.path);
        }
        self.dirty = true;
    }

    pub fn project_for_cwd(&self, cwd: &Path) -> Option<Id> {
        project_for(&self.data.projects, cwd)
    }

    fn remap_sessions(&mut self) {
        let projects = self.data.projects.clone();
        for s in &mut self.data.sessions {
            if s.project_id.is_none() {
                s.project_id = project_for(&projects, &s.cwd);
            }
        }
    }

    pub fn add_session(&mut self, record: SessionRecord) {
        self.data.sessions.push(record);
        self.dirty = true;
    }

    /// Merge a fresh scan of Claude Code's records into our index. See [`reconcile`].
    pub fn reconcile(&mut self, external: &[ExternalSession], scan_ok: bool) -> ReconcileStats {
        self.dirty = true;
        reconcile(&mut self.data, external, scan_ok)
    }
}

/// Deepest project containing `cwd` (project root or any of its worktrees).
pub fn project_for(projects: &[Project], cwd: &Path) -> Option<Id> {
    projects
        .iter()
        .filter_map(|p| {
            let mut best: Option<usize> = None;
            let roots = std::iter::once(&p.path).chain(p.git.iter().flat_map(|g| g.worktrees.iter()));
            for root in roots {
                if is_same_or_under(cwd, root) {
                    let depth = path_key(root).len();
                    best = Some(best.map_or(depth, |b: usize| b.max(depth)));
                }
            }
            best.map(|d| (d, p.id.clone()))
        })
        .max_by_key(|(d, _)| *d)
        .map(|(_, id)| id)
}

/// What we learned about one Claude Code session from its on-disk record.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalSession {
    pub session_id: String,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    pub created_at: i64,
    pub last_active: i64,
    pub model: Option<String>,
    pub context_tokens: Option<u64>,
    pub cache_ttl: Option<u64>,
    pub cache_at: Option<i64>,
    pub transcript: PathBuf,
}

#[derive(Debug, Default, PartialEq)]
pub struct ReconcileStats {
    pub added: usize,
    pub updated: usize,
    pub marked_missing: usize,
}

/// Startup reconciliation (CLAUDE.md "Session import and synchronization"):
/// - external records are matched by Claude session id, so sessions Claudiu launched itself never duplicate;
/// - Claudiu-only metadata (custom title, hidden, project) is preserved;
/// - nothing is ever deleted: absent records are flagged `Missing`, and only when the scan itself succeeded.
pub fn reconcile(data: &mut Data, external: &[ExternalSession], scan_ok: bool) -> ReconcileStats {
    let mut stats = ReconcileStats::default();
    let projects = data.projects.clone();

    for ext in external {
        let existing = data.sessions.iter_mut().find(|s| s.claude_id() == Some(ext.session_id.as_str()));
        match existing {
            Some(rec) => {
                let before = rec.clone();
                if let Some(title) = ext.title.as_ref().filter(|t| !t.trim().is_empty()) {
                    rec.title = title.clone();
                }
                rec.last_active = rec.last_active.max(ext.last_active);
                if rec.project_id.is_none()
                    && let Some(cwd) = &ext.cwd {
                        rec.project_id = project_for(&projects, cwd);
                    }
                let meta = rec.claude.get_or_insert_with(|| ClaudeMeta { session_id: ext.session_id.clone(), ..Default::default() });
                meta.external = ExternalState::Present;
                meta.transcript = Some(ext.transcript.clone());
                if ext.model.is_some() {
                    meta.model = ext.model.clone();
                }
                if ext.context_tokens.is_some() {
                    meta.context_tokens = ext.context_tokens;
                }
                if ext.cache_ttl.is_some() {
                    meta.cache_ttl = ext.cache_ttl;
                }
                if ext.cache_at.is_some() {
                    meta.cache_at = ext.cache_at;
                }
                if *rec != before {
                    stats.updated += 1;
                }
            }
            None => {
                let Some(cwd) = ext.cwd.clone() else { continue };
                data.sessions.push(SessionRecord {
                    id: new_id(),
                    kind: SessionKind::Claude,
                    title: ext.title.clone().unwrap_or_else(|| "Untitled session".into()),
                    custom_title: None,
                    auto_title: None,
                    project_id: project_for(&projects, &cwd),
                    cwd,
                    created_at: ext.created_at,
                    last_active: ext.last_active,
                    exit_code: None,
                    hidden: false,
                    claude: Some(ClaudeMeta {
                        session_id: ext.session_id.clone(),
                        model: ext.model.clone(),
                        context_tokens: ext.context_tokens,
                        transcript: Some(ext.transcript.clone()),
                        external: ExternalState::Present,
                        ..Default::default()
                    }),
                });
                stats.added += 1;
            }
        }
    }

    if scan_ok {
        for rec in &mut data.sessions {
            let Some(meta) = rec.claude.as_mut() else { continue };
            if meta.external == ExternalState::Present && !external.iter().any(|e| e.session_id == meta.session_id) {
                meta.external = ExternalState::Missing;
                stats.marked_missing += 1;
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(id: &str, cwd: &str, title: &str, last: i64) -> ExternalSession {
        ExternalSession {
            session_id: id.into(),
            cwd: Some(PathBuf::from(cwd)),
            title: Some(title.into()),
            created_at: 1,
            last_active: last,
            model: Some("claude-sonnet-5-5".into()),
            context_tokens: Some(1000),
            cache_ttl: None,
            cache_at: None,
            transcript: PathBuf::from(format!("/t/{id}.jsonl")),
        }
    }

    fn project(id: &str, path: &str) -> Project {
        Project { id: id.into(), name: id.into(), path: PathBuf::from(path), git: None, collapsed: false, added_at: 0 }
    }

    #[test]
    fn imports_new_sessions_and_maps_to_deepest_project() {
        let mut d = Data { projects: vec![project("outer", "/w"), project("inner", "/w/app")], ..Default::default() };
        let stats = reconcile(&mut d, &[ext("a", "/w/app/src", "T", 10), ext("b", "/other", "U", 5)], true);
        assert_eq!(stats.added, 2);
        assert_eq!(d.sessions[0].project_id.as_deref(), Some("inner"));
        assert_eq!(d.sessions[1].project_id, None);
    }

    #[test]
    fn rerunning_is_idempotent_and_never_duplicates() {
        let mut d = Data::default();
        let e = [ext("a", "/w", "T", 10)];
        reconcile(&mut d, &e, true);
        let stats = reconcile(&mut d, &e, true);
        assert_eq!(d.sessions.len(), 1);
        assert_eq!(stats, ReconcileStats::default());
    }

    #[test]
    fn session_launched_by_claudiu_is_matched_not_duplicated() {
        let mut d = Data::default();
        d.sessions.push(SessionRecord {
            id: "local".into(),
            kind: SessionKind::Claude,
            title: "Claude".into(),
            custom_title: Some("My name".into()),
            auto_title: None,
            cwd: PathBuf::from("/w"),
            project_id: None,
            created_at: 1,
            last_active: 2,
            exit_code: Some(0),
            hidden: true,
            claude: Some(ClaudeMeta { session_id: "a".into(), ..Default::default() }),
        });
        reconcile(&mut d, &[ext("a", "/w", "Ext title", 50)], true);
        assert_eq!(d.sessions.len(), 1);
        let s = &d.sessions[0];
        assert_eq!(s.id, "local");
        assert_eq!(s.title, "Ext title");
        assert_eq!(s.display_title(), "My name", "Claudiu-only label survives");
        assert!(s.hidden, "hidden flag survives");
        assert_eq!(s.last_active, 50);
        assert_eq!(s.claude.as_ref().unwrap().external, ExternalState::Present);
    }

    #[test]
    fn missing_external_is_flagged_not_deleted_and_only_on_good_scan() {
        let mut d = Data::default();
        reconcile(&mut d, &[ext("a", "/w", "T", 1)], true);

        let stats = reconcile(&mut d, &[], false);
        assert_eq!(stats.marked_missing, 0, "unreadable scan must not change state");
        assert_eq!(d.sessions[0].claude.as_ref().unwrap().external, ExternalState::Present);

        let stats = reconcile(&mut d, &[], true);
        assert_eq!(stats.marked_missing, 1);
        assert_eq!(d.sessions.len(), 1, "history is never deleted");
        assert_eq!(d.sessions[0].claude.as_ref().unwrap().external, ExternalState::Missing);

        // It comes back if the file reappears.
        reconcile(&mut d, &[ext("a", "/w", "T", 1)], true);
        assert_eq!(d.sessions[0].claude.as_ref().unwrap().external, ExternalState::Present);
    }

    #[test]
    fn shell_sessions_are_untouched() {
        let mut d = Data::default();
        d.sessions.push(SessionRecord {
            id: "sh".into(),
            kind: SessionKind::Shell,
            title: "PowerShell".into(),
            custom_title: None,
            auto_title: None,
            cwd: PathBuf::from("/w"),
            project_id: None,
            created_at: 1,
            last_active: 1,
            exit_code: None,
            hidden: false,
            claude: None,
        });
        let stats = reconcile(&mut d, &[], true);
        assert_eq!(stats, ReconcileStats::default());
        assert_eq!(d.sessions.len(), 1);
    }

    #[test]
    fn adding_a_project_adopts_existing_unmapped_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::load(tmp.path().join("state.json"));
        let proj_dir = tmp.path().join("proj");
        fs::create_dir_all(&proj_dir).unwrap();
        let e = ext("a", &proj_dir.join("sub").to_string_lossy(), "T", 1);
        store.reconcile(&[e], true);
        assert!(store.data.sessions[0].project_id.is_none());
        let id = store.add_project(&proj_dir);
        assert_eq!(store.data.sessions[0].project_id.as_deref(), Some(id.as_str()));
        assert_eq!(store.add_project(&proj_dir), id, "idempotent");
    }

    #[test]
    fn shell_sessions_are_not_restored_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.json");
        let mut a = Store::load(path.clone());
        for (id, kind) in [("sh", SessionKind::Shell), ("cl", SessionKind::Claude)] {
            a.data.sessions.push(SessionRecord {
                id: id.into(),
                kind,
                title: id.into(),
                custom_title: None,
                auto_title: None,
                cwd: PathBuf::from("/w"),
                project_id: None,
                created_at: 1,
                last_active: 1,
                exit_code: None,
                hidden: false,
                claude: None,
            });
        }
        a.save();
        let b = Store::load(path);
        assert_eq!(b.data.sessions.len(), 1);
        assert_eq!(b.data.sessions[0].id, "cl");
    }

    #[test]
    fn peak_usage_ignores_windows_that_have_reset() {
        let lim = |u: f32, reset: Option<i64>| Some(RateLimit { utilization: Some(u), resets_at: reset, seen_at: 0 });
        let u = AccountUsage { five_hour: lim(0.9, Some(100)), seven_day: lim(0.4, Some(1000)) };
        assert_eq!(u.peak(50), 0.9);
        assert_eq!(u.peak(500), 0.4, "the 5-hour window reset");
        assert_eq!(u.peak(5000), 0.0);
        assert_eq!(AccountUsage::default().peak(0), 0.0, "unknown counts as unused");
    }

    #[test]
    fn usage_from_before_accounts_moves_into_the_main_account() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.json");
        let old = r#"{"version":1,"projects":[],"sessions":[],"account":{"five_hour":{"utilization":0.5,"resets_at":null,"seen_at":3}},
            "layout":{"selected_session":null,"sidebar_visible":true,"other_collapsed":false},"settings":{"claude_path":null,"update_url":null}}"#;
        fs::write(&path, old).unwrap();
        let mut s = Store::load(path.clone());
        assert_eq!(s.active_account().unwrap().usage.five_hour.as_ref().unwrap().utilization, Some(0.5));
        s.save();
        assert!(fs::read_to_string(&path).unwrap().contains("\"account\""), "older versions require the field");
        assert_eq!(Store::load(path).data.accounts.len(), 1, "migrates once");
    }

    #[test]
    fn save_load_roundtrip_and_corrupt_file_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.json");
        let mut a = Store::load(path.clone());
        a.data.projects.push(project("p", "/x"));
        a.save();
        let b = Store::load(path.clone());
        assert_eq!(b.data.projects.len(), 1);

        fs::write(&path, "{ not json").unwrap();
        let c = Store::load(path.clone());
        assert!(c.data.projects.is_empty());
        assert_eq!(c.active_account().map(|a| a.id.as_str()), Some(MAIN_ACCOUNT), "main account always exists");
        assert!(path.with_extension("json.corrupt").exists(), "corrupt state is preserved, not destroyed");
    }
}
