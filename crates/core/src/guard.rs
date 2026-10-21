//! The two guards: writes outside the workspace, and a short list of dangerous commands.
//!
//! These are guardrails, not isolation: file tools are checked exactly, shell commands on a best-effort reading
//! of the command line (a command that builds its target from a variable or a subshell is not understood and is
//! let through). Every refusal is one line that names the way forward.

use std::path::{Component, Path, PathBuf};

/// Directories a task may write besides its project: the usual toolchain caches and scratch space.
#[derive(Clone, Debug)]
pub struct GuardConfig {
    pub toolchain_dirs: Vec<PathBuf>,
}

impl Default for GuardConfig {
    fn default() -> Self {
        let home = home_dir();
        let under = |rel: &str| home.as_ref().map(|h| h.join(rel));
        let mut dirs: Vec<PathBuf> = [".cargo", ".rustup", ".npm", ".cache", ".gradle", ".m2", ".nuget", ".claude/plans"].iter().filter_map(|r| under(r)).collect();
        dirs.push(std::env::temp_dir());
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("npm-cache"));
        }
        GuardConfig { toolchain_dirs: dirs }
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// Lexically cleans `path` against `cwd` and resolves symlinks of whatever part of it exists.
pub fn resolve(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    let mut clean = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    // Resolve links through the deepest ancestor that exists, so a link inside the project cannot lead out of it.
    let mut tail = Vec::new();
    let mut probe = clean.clone();
    loop {
        if let Ok(real) = std::fs::canonicalize(&probe) {
            let mut out = real;
            out.extend(tail.iter().rev());
            return out;
        }
        match (probe.file_name().map(|n| n.to_os_string()), probe.parent().map(Path::to_path_buf)) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                probe = parent;
            }
            _ => return clean,
        }
    }
}

/// A comparable form: no verbatim prefix, forward slashes, no trailing slash, case-folded where the file system is.
fn key(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut text = text.replace('\\', "/");
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    if cfg!(any(windows, target_os = "macos")) { text.to_lowercase() } else { text }
}

pub fn is_within(path: &Path, root: &Path) -> bool {
    let (p, r) = (key(path), key(root));
    p == r || p.starts_with(&format!("{}/", r.trim_end_matches('/')))
}

/// Where a write may go: the project, the toolchain caches, and what the user granted.
pub struct Allowed<'a> {
    pub cwd: &'a Path,
    pub toolchain_dirs: &'a [PathBuf],
    pub grants: &'a [PathBuf],
}

impl Allowed<'_> {
    pub fn permits(&self, target: &Path) -> bool {
        let target = resolve(target, self.cwd);
        let roots = std::iter::once(self.cwd).chain(self.toolchain_dirs.iter().map(PathBuf::as_path)).chain(self.grants.iter().map(PathBuf::as_path));
        roots.map(|r| resolve(r, self.cwd)).any(|root| is_within(&target, &root))
    }

    pub fn refusal(&self, target: &Path) -> String {
        format!(
            "Refused: {} is outside the project. Write inside {} instead, or call request_access with the path and a reason.",
            target.display(),
            self.cwd.display()
        )
    }
}
