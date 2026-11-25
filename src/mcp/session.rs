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
    cwd: PathBuf,
    project: Option<PathBuf>,
    data: PathBuf,
}

impl SessionTools {
    pub fn new(record: &str, cwd: &Path, project: Option<&Path>, off: &[&str], targets: &[&str]) -> Result<Self, String> {
        let executable = mcp_executable().map_err(|e| format!("Could not locate m4code's MCP executable: {e}"))?;
        let token = uuid::Uuid::new_v4();
        let data = std::path::absolute(crate::datadir::data_dir()).map_err(|e| format!("Could not resolve m4code's data directory: {e}"))?;
        let ready = Ready(ready_path(&data, token));
        let env = vec![
            (RECORD_ENV, record.into()),
            (TOOLS_OFF_ENV, off.join(",")),
            ("M4CODE_DATA_DIR", data.to_string_lossy().into_owned()),
            (TARGETS_ENV, targets.join(",")),
            (READY_ENV, token.to_string()),
        ];
        Ok(Self { executable, env, ready, cwd: cwd.to_path_buf(), project: project.map(Path::to_path_buf), data })
    }

    pub fn load_workspace(&mut self) {
        self.env.push((WORKSPACE_ENV, workspace(&self.cwd, self.project.as_deref()).to_string()));
    }

    pub fn renew(&mut self) {
        let token = uuid::Uuid::new_v4();
        self.ready = Ready(ready_path(&self.data, token));
        if let Some((_, value)) = self.env.iter_mut().find(|(key, _)| *key == READY_ENV) {
            *value = token.to_string();
        }
    }

    pub fn stdio(&self) -> Value {
        crate::harness::stdio_server("m4code", &self.executable, &[FLAG], &self.env)
    }

    pub fn can_signal_ready(&self) -> bool {
        #[cfg(test)]
        {
            !is_cargo_test_binary(&self.executable)
        }
        #[cfg(not(test))]
        {
            true
        }
    }
}

fn mcp_executable() -> std::io::Result<PathBuf> {
    let exe = crate::platform::own_exe()?;
    #[cfg(test)]
    if is_cargo_test_binary(&exe)
        && let Some(dir) = exe.parent().and_then(Path::parent)
    {
        let bin = dir.join(if cfg!(windows) { "m4code.exe" } else { "m4code" });
        if bin.is_file() {
            return Ok(bin);
        }
    }
    Ok(exe)
}

#[cfg(test)]
fn is_cargo_test_binary(exe: &Path) -> bool {
    exe.parent().and_then(Path::file_name).is_some_and(|dir| dir == "deps")
}

pub fn workspace(cwd: &Path, project: Option<&Path>) -> Value {
    let cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let git = crate::git::workspace_details(&cwd);
    json!({
        "working_directory": cwd.to_string_lossy(),
        "project_root": project.or_else(|| git.as_ref().map(|(_, project, _)| project.as_path())).map(Path::to_string_lossy),
        "worktree": git.as_ref().map(|(checkout, _, _)| checkout.to_string_lossy()),
        "branch": git.as_ref().and_then(|(_, _, branch)| branch.as_deref()),
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

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Ready {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
