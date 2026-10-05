//! Git awareness via libgit2 (`git2`): repository root, current branch, linked worktrees.

use std::path::{Path, PathBuf};

use git2::Repository;
use serde::{Deserialize, Serialize};

use crate::platform::{normalize, path_key};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GitInfo {
    /// Working tree root that contains `.git`.
    pub root: PathBuf,
    pub branch: Option<String>,
    /// Other working trees attached to the same repository (excluding `root`).
    pub worktrees: Vec<PathBuf>,
    /// True when `root` is itself a linked worktree rather than the main checkout.
    pub is_worktree: bool,
}

/// Find the repository containing `path`, if any.
pub fn detect(path: &Path) -> Option<GitInfo> {
    let repo = Repository::discover(path).ok()?;
    let root = normalize(repo.workdir()?);
    let is_worktree = repo.is_worktree();

    let branch = match repo.head() {
        Ok(head) if head.is_branch() => head.shorthand().ok().map(str::to_string),
        Ok(head) => head.target().map(|oid| format!("detached {}", &oid.to_string()[..7])),
        // Fresh repo without commits: HEAD is symbolic but unborn.
        Err(_) => repo
            .find_reference("HEAD")
            .ok()
            .and_then(|r| r.symbolic_target().ok().flatten().map(|t| t.trim_start_matches("refs/heads/").to_string())),
    };

    // Worktrees are enumerated from the main repository.
    let main = if is_worktree { Repository::open(repo.commondir()).ok() } else { None };
    let main_repo = main.as_ref().unwrap_or(&repo);
    let mut worktrees: Vec<PathBuf> = Vec::new();
    if is_worktree {
        if let Some(wd) = main_repo.workdir() {
            worktrees.push(normalize(wd));
        }
    }
    if let Ok(names) = main_repo.worktrees() {
        for name in names.iter().flatten().flatten() {
            if let Ok(wt) = main_repo.find_worktree(name) {
                worktrees.push(normalize(wt.path()));
            }
        }
    }
    worktrees.retain(|p| path_key(p) != path_key(&root));
    worktrees.sort();
    worktrees.dedup();
    Some(GitInfo { root, branch, worktrees, is_worktree })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_with_commit(path: &Path) -> Repository {
        let repo = Repository::init(path).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "t").unwrap();
            cfg.set_str("user.email", "t@t").unwrap();
        }
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        drop(tree);
        repo
    }

    #[test]
    fn plain_repo_from_subfolder() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("proj");
        fs::create_dir_all(repo_dir.join("src/deep")).unwrap();
        init_with_commit(&repo_dir);

        let info = detect(&repo_dir.join("src/deep")).unwrap();
        assert_eq!(path_key(&info.root), path_key(&repo_dir));
        assert!(info.branch.is_some());
        assert!(info.worktrees.is_empty() && !info.is_worktree);
    }

    #[test]
    fn unborn_branch_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        Repository::init(tmp.path()).unwrap();
        assert!(detect(tmp.path()).unwrap().branch.is_some());
    }

    #[test]
    fn linked_worktrees_are_discovered_from_both_sides() {
        let tmp = tempfile::tempdir().unwrap();
        let main_dir = tmp.path().join("main");
        let wt_dir = tmp.path().join("wt");
        fs::create_dir_all(&main_dir).unwrap();
        let repo = init_with_commit(&main_dir);
        repo.worktree("wt", &wt_dir, None).unwrap();

        let from_main = detect(&main_dir).unwrap();
        assert_eq!(from_main.worktrees.len(), 1);
        assert_eq!(path_key(&from_main.worktrees[0]), path_key(&wt_dir));

        let from_wt = detect(&wt_dir).unwrap();
        assert!(from_wt.is_worktree);
        assert_eq!(from_wt.worktrees.len(), 1, "main checkout is listed: {:?}", from_wt.worktrees);
        assert_eq!(path_key(&from_wt.worktrees[0]), path_key(&main_dir));
    }

    #[test]
    fn not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        // tempdir lives under the user's temp folder, which is not inside a repository.
        assert!(detect(tmp.path()).is_none());
    }
}
