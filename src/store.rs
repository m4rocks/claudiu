//! m4code's own persisted state (projects, session history, cached usage, layout).
//! Independent of GPUI. Claude Code's data is only ever *read* (see `claude/`) and merged in here.

use std::collections::BTreeMap;
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

mod sessions;
pub use sessions::{SessionKind, ExternalState, Attached, ClaudeMeta, ProviderSession, SessionRecord, Project};
mod accounts;
pub use accounts::{RateLimit, AccountUsage, Account, MAIN_ACCOUNT};
mod settings;
pub use settings::{WindowGeom, Layout, Settings};
mod data;
pub use data::{Data};

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
    writer: Writer,
}

/// Writes the state files on one background thread, in the order they were handed over, so the UI never waits on the
/// disk and an older copy never lands after a newer one. It skips a write that would leave a file as it already is.
struct Writer {
    jobs: std::sync::mpsc::Sender<Job>,
    /// Set by the thread when a write fails: the store's data is not all on disk, and the next save writes it again.
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

enum Job {
    Write(Vec<(PathBuf, String)>),
    /// Answered once every write handed over before it has been made.
    Flush(std::sync::mpsc::Sender<()>),
}

impl Writer {
    fn start() -> Self {
        let (jobs, queue) = std::sync::mpsc::channel::<Job>();
        let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failing = failed.clone();
        // It ends once the store is dropped and what was handed over is written.
        std::thread::spawn(move || {
            let mut on_disk: std::collections::HashMap<PathBuf, String> = Default::default();
            for job in queue {
                match job {
                    Job::Write(files) => {
                        for (path, text) in files {
                            if on_disk.get(&path) == Some(&text) {
                                continue;
                            }
                            if write_atomic(&path, &text) {
                                on_disk.insert(path, text);
                            } else {
                                on_disk.remove(&path);
                                failing.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
                    }
                    Job::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self { jobs, failed }
    }
}

/// Written beside the file and moved over it, so a crash mid-write never leaves half a file.
fn write_atomic(path: &Path, text: &str) -> bool {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).is_ok() && fs::rename(&tmp, path).is_ok()
}

fn migrate_data_dir(dir: &Path, old_dirs: &[PathBuf]) {
    if dir.join("state.json").exists() {
        return;
    }
    for old in old_dirs {
        if old.join("state.json").exists() {
            // Same volume, so a rename moves everything at once; if it fails the app starts fresh, the old folder untouched.
            if dir.exists() {
                let _ = fs::remove_dir(dir);
            }
            if fs::create_dir_all(dir.parent().unwrap_or(dir)).and_then(|_| fs::rename(old, dir)).is_ok() {
                break;
            }
        }
    }
}

impl Store {
    /// Platform app-data location, e.g. `%LOCALAPPDATA%\m4rocks\m4code\data\state.json` (`m4code Dev` in a debug build).
    pub fn default_path() -> PathBuf {
        // Tests (and anyone running a second copy) can keep their state away from the real one.
        if let Some(dir) = std::env::var_os("M4CODE_DATA_DIR") {
            return PathBuf::from(dir).join("state.json");
        }
        // Local (non-roaming) data: it holds machine-specific paths.
        // On Windows the Velopack install root is `%LOCALAPPDATA%\m4code`, and running Setup.exe over it wipes
        // everything inside, so the data lives under an organization folder beside it instead.
        let org = if cfg!(windows) { "m4rocks" } else { "" };
        // A debug build (`cargo run`) keeps its own folder: two apps on one folder take each other's request files (#84).
        let app = if cfg!(debug_assertions) { "m4code Dev" } else { "m4code" };
        let Some(dirs) = directories::ProjectDirs::from("", org, app) else { return PathBuf::from("m4code-state.json") };
        let dir = dirs.data_local_dir().to_path_buf();
        static MIGRATE: std::sync::Once = std::sync::Once::new();
        MIGRATE.call_once(|| {
            // Only the released app has data at the old Claudiu locations; a debug build must not move it.
            if cfg!(debug_assertions) {
                return;
            }
            let mut old_dirs = Vec::new();
            if let Some(d) = directories::ProjectDirs::from("", org, "Claudiu") {
                old_dirs.push(d.data_local_dir().to_path_buf());
            }
            #[cfg(windows)]
            if let Some(d) = directories::ProjectDirs::from("", "", "Claudiu") {
                old_dirs.push(d.data_local_dir().to_path_buf());
            }
            migrate_data_dir(&dir, &old_dirs);
        });
        dir.join("state.json")
    }

    /// No state file yet: m4code has never run here. Ask before anything calls `load`, which moves a corrupt file aside
    /// (a corrupt file still means the user has been here).
    pub fn is_first_launch(path: &Path) -> bool {
        !path.exists()
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
        Self { path, data, dirty: false, writer: Writer::start() }
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.data.accounts.iter().find(|a| a.id == id)
    }

    /// The account new sessions use; falls back to the main one if the chosen alt was removed.
    pub fn active_account(&self) -> Option<&Account> {
        self.data.active_account.as_deref().and_then(|id| self.account(id)).or_else(|| self.account(MAIN_ACCOUNT))
    }

    /// The account a session starts under: its own while that account exists, else the active one.
    pub fn session_account(&self, session_id: &str) -> Option<&Account> {
        self.session(session_id)
            .and_then(|r| r.claude.as_ref()?.account.as_deref())
            .and_then(|id| self.account(id))
            .or_else(|| self.active_account())
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Hands the data to the background writer when it changed since the last save (or that save failed); the call
    /// returns before anything reaches the disk.
    pub fn save_if_dirty(&mut self) {
        if self.writer.failed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.dirty = true;
        }
        if self.dirty {
            self.hand_over();
        }
    }

    /// Writes the data and waits until it is on disk, for when the process may end right after (quitting).
    pub fn save(&mut self) {
        self.hand_over();
        let (done, wait) = std::sync::mpsc::channel();
        if self.writer.jobs.send(Job::Flush(done)).is_err() || wait.recv().is_err() || self.writer.failed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.dirty = true;
        }
    }

    fn hand_over(&mut self) {
        let accounts = AccountsFile { accounts: self.data.accounts.clone(), active: self.data.active_account.clone() };
        let (Ok(accounts), Ok(state)) = (serde_json::to_string_pretty(&accounts), serde_json::to_string_pretty(&self.data)) else { return };
        let files = vec![(accounts_path(&self.path), accounts), (self.path.clone(), state)];
        self.dirty = self.writer.jobs.send(Job::Write(files)).is_err();
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
        let name = folder_name(&root);
        let project = Project { id: new_id(), name, path: root, git, collapsed: true, added_at: now() };
        let id = project.id.clone();
        self.data.projects.push(project);
        self.remap_sessions();
        self.dirty = true;
        id
    }

    /// Drop a project; its sessions stay, without one. Returns where it was and what it was, for `restore_project`.
    pub fn remove_project(&mut self, id: &str) -> Option<(usize, Project)> {
        let at = self.data.projects.iter().position(|p| p.id == id)?;
        let project = self.data.projects.remove(at);
        for s in self.data.sessions.iter_mut().filter(|s| s.project_id.as_deref() == Some(id)) {
            s.project_id = None;
        }
        self.dirty = true;
        Some((at, project))
    }

    /// Put a removed project back at `at`, taking back the sessions in its folder. Nothing happens when its folder has
    /// been added again since.
    pub fn restore_project(&mut self, at: usize, project: Project) {
        if self.data.projects.iter().any(|p| path_key(&p.path) == path_key(&project.path)) {
            return;
        }
        self.data.projects.insert(at.min(self.data.projects.len()), project);
        self.remap_sessions();
        self.dirty = true;
    }

    /// The name the sidebar shows for a project; an empty one goes back to its folder's name.
    pub fn rename_project(&mut self, id: &str, name: &str) {
        let Some(p) = self.data.projects.iter_mut().find(|p| p.id == id) else { return };
        let name = name.trim();
        p.name = if name.is_empty() { folder_name(&p.path) } else { name.to_string() };
        self.dirty = true;
    }

    /// Move a project one place up or down the list; the first and last stay where they are.
    pub fn move_project(&mut self, id: &str, up: bool) {
        let Some(at) = self.data.projects.iter().position(|p| p.id == id) else { return };
        let to = if up { at.checked_sub(1) } else { Some(at + 1).filter(|&to| to < self.data.projects.len()) };
        if let Some(to) = to {
            self.data.projects.swap(at, to);
            self.dirty = true;
        }
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
}

/// Deepest project containing `cwd` (project root or any of its worktrees).
/// A project's name before anyone renames it.
fn folder_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned())
}

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

/// The `models` whose id is in `picked`, in their own order. With nothing picked, or nothing picked that is still
/// offered, all of them: a harness never drops out of the picker because of stale ticks.
pub fn keep_picked<T>(models: Vec<T>, picked: &[String], id: impl Fn(&T) -> &str) -> Vec<T> {
    if !models.iter().any(|m| picked.iter().any(|p| p == id(m))) {
        return models;
    }
    models.into_iter().filter(|m| picked.iter().any(|p| p == id(m))).collect()
}

#[cfg(test)]
mod tests;
