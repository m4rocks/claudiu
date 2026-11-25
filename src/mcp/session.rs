//! The per-session MCP environment shared by foreground threads and native child runners.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::{FLAG, RECORD_ENV, TOOLS_OFF_ENV};

pub const WORKSPACE_ENV: &str = "M4CODE_WORKSPACE";
pub const TARGETS_ENV: &str = "M4CODE_SUBAGENT_TARGETS";
pub const READY_ENV: &str = "M4CODE_MCP_READY";
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub struct SessionTools {
    pub executable: PathBuf,
    pub env: Vec<(&'static str, String)>,
    pub ready: Ready,
}

impl SessionTools {
    pub fn new(record: &str, cwd: &Path, project: Option<&Path>, off: &[&str], targets: &[&str]) -> Result<Self, String> {
        let executable = crate::platform::own_exe().map_err(|e| format!("Could not locate m4code's MCP executable: {e}"))?;
        let token = uuid::Uuid::new_v4();
        let ready = Ready(ready_path(&crate::datadir::data_dir(), token));
        let env = vec![
            (RECORD_ENV, record.into()),
            (TOOLS_OFF_ENV, off.join(",")),
            (WORKSPACE_ENV, workspace(cwd, project).to_string()),
            (TARGETS_ENV, targets.join(",")),
            (READY_ENV, token.to_string()),
        ];
        Ok(Self { executable, env, ready })
    }

    pub fn renew(&mut self) {
        let token = uuid::Uuid::new_v4();
        self.ready = Ready(ready_path(&crate::datadir::data_dir(), token));
        if let Some((_, value)) = self.env.iter_mut().find(|(key, _)| *key == READY_ENV) {
            *value = token.to_string();
        }
    }

    pub fn stdio(&self) -> Value {
        crate::harness::stdio_server("m4code", &self.executable, &[FLAG], &self.env)
    }
}

pub fn workspace(cwd: &Path, project: Option<&Path>) -> Value {
    let cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let git = crate::git::detect(&cwd);
    json!({
        "working_directory": cwd.to_string_lossy(),
        "project_root": project.or_else(|| git.as_ref().map(|g| g.main_checkout())).map(Path::to_string_lossy),
        "worktree": git.as_ref().map(|g| g.root.to_string_lossy()),
        "branch": git.as_ref().and_then(|g| g.branch.as_deref()),
    })
}

pub fn ready_path(data: &Path, token: uuid::Uuid) -> PathBuf {
    data.join("mcp-ready").join(token.to_string())
}

#[derive(Debug)]
pub struct Ready(PathBuf);

impl Ready {
    pub fn observed(&self) -> bool {
        self.0.is_file()
    }
}

impl Drop for Ready {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
