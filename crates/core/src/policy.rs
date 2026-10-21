//! What the user has allowed beyond the project, kept in small JSON files the guard hooks read on every call.
//!
//! A grant covers one task or the whole project (every task started in the same folder). Approving edits a file, so
//! it takes effect on the next call with nothing to relaunch.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::event::SessionId;
use crate::guard::resolve;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Task,
    Project,
}

pub struct Policy {
    dir: PathBuf,
}

impl Policy {
    pub fn new(dir: PathBuf) -> Policy {
        Policy { dir }
    }

    fn file(&self, scope: Scope, session: &SessionId, cwd: &Path) -> PathBuf {
        match scope {
            Scope::Task => self.dir.join(format!("task-{session}.json")),
            Scope::Project => {
                let digest = Sha256::digest(resolve(cwd, cwd).to_string_lossy().to_lowercase().as_bytes());
                let name: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
                self.dir.join(format!("project-{name}.json"))
            }
        }
    }

    fn read(file: &Path) -> Vec<PathBuf> {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        value["paths"].as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(PathBuf::from)).collect()).unwrap_or_default()
    }

    /// Every extra path this task may write: its own grants and its project's.
    pub fn grants(&self, session: &SessionId, cwd: &Path) -> Vec<PathBuf> {
        let mut all = Policy::read(&self.file(Scope::Task, session, cwd));
        all.extend(Policy::read(&self.file(Scope::Project, session, cwd)));
        all
    }

    pub fn grant(&self, scope: Scope, session: &SessionId, cwd: &Path, path: &Path) -> std::io::Result<()> {
        let file = self.file(scope, session, cwd);
        let mut paths = Policy::read(&file);
        if !paths.iter().any(|p| p == path) {
            paths.push(path.to_path_buf());
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, json!({ "paths": paths }).to_string())?;
        std::fs::rename(tmp, file)
    }
}
