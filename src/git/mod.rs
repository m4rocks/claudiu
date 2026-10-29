//! Git awareness via libgit2 (`git2`): repository root, current branch, linked worktrees.
//! `commit` is the Commit & Push flow built on top of it.

pub mod commit;
pub mod lines;
pub mod worktree;

use std::path::{Path, PathBuf};

use git2::Repository;
use serde::{Deserialize, Serialize};

use crate::platform::{is_same_or_under, normalize, path_key};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GitInfo {
    /// Working tree root that contains `.git`.
    pub root: PathBuf,
    pub branch: Option<String>,
    /// Other working trees attached to the same repository (excluding `root`).
    pub worktrees: Vec<PathBuf>,
    /// True when `root` is itself a linked worktree rather than the main checkout.
    pub is_worktree: bool,
    /// The repository's main checkout; `root` unless that is a linked worktree. Empty in state saved before this
    /// field existed, until the next refresh.
    #[serde(default)]
    pub main: PathBuf,
    /// The branch checked out in each of `worktrees`. A worktree whose branch can't be read is left out.
    #[serde(default)]
    pub worktree_branches: Vec<(PathBuf, String)>,
    /// The repository has no commit yet, so a worktree has nothing to start from.
    #[serde(default)]
    pub unborn: bool,
}

impl GitInfo {
    /// The main checkout, also for state saved before `main` existed.
    pub fn main_checkout(&self) -> &Path {
        if self.main.as_os_str().is_empty() { &self.root } else { &self.main }
    }

    /// The working tree (`root` or one of `worktrees`) that holds `cwd`. A worktree inside the main checkout, such as
    /// Claude Code's `.claude/worktrees/<name>`, wins over the checkout around it.
    pub fn folder_of(&self, cwd: &Path) -> Option<&Path> {
        std::iter::once(&self.root)
            .chain(&self.worktrees)
            .filter(|folder| is_same_or_under(cwd, folder))
            .max_by_key(|folder| path_key(folder).len())
            .map(PathBuf::as_path)
    }

    /// The branch checked out in a working tree from `folder_of`.
    pub fn branch_in(&self, folder: &Path) -> Option<&str> {
        if path_key(folder) == path_key(&self.root) {
            return self.branch.as_deref();
        }
        let (_, branch) = self.worktree_branches.iter().find(|(path, _)| path_key(path) == path_key(folder))?;
        Some(branch)
    }

    /// The branch of the working tree that holds `cwd`.
    pub fn branch_at(&self, cwd: &Path) -> Option<&str> {
        self.folder_of(cwd).map_or(self.branch.as_deref(), |folder| self.branch_in(folder))
    }

    pub fn is_main(&self, folder: &Path) -> bool {
        path_key(folder) == path_key(self.main_checkout())
    }
}

/// What `HEAD` of a repository is called: the branch, or "detached <commit>".
fn head_name(repo: &Repository) -> Option<String> {
    match repo.head() {
        Ok(head) if head.is_branch() => head.shorthand().ok().map(str::to_string),
        Ok(head) => head.target().map(|oid| format!("detached {}", &oid.to_string()[..7])),
        // Fresh repo without commits: HEAD is symbolic but unborn.
        Err(_) => repo
            .find_reference("HEAD")
            .ok()
            .and_then(|r| r.symbolic_target().ok().flatten().map(|t| t.trim_start_matches("refs/heads/").to_string())),
    }
}

/// Find the repository containing `path`, if any.
pub fn detect(path: &Path) -> Option<GitInfo> {
    let repo = Repository::discover(path).ok()?;
    let root = normalize(repo.workdir()?);
    let is_worktree = repo.is_worktree();
    let branch = head_name(&repo);

    // Worktrees are enumerated from the main repository.
    let main = if is_worktree { Repository::open(repo.commondir()).ok() } else { None };
    let main_repo = main.as_ref().unwrap_or(&repo);
    let main_dir = main_repo.workdir().map_or_else(|| root.clone(), normalize);
    let mut worktrees: Vec<PathBuf> = Vec::new();
    if is_worktree {
        worktrees.push(main_dir.clone());
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
    let worktree_branches = worktrees
        .iter()
        .filter_map(|path| Some((path.clone(), head_name(&Repository::open(path).ok()?)?)))
        .collect();
    let unborn = repo.is_empty().unwrap_or(false);
    Some(GitInfo { root, branch, worktrees, is_worktree, main: main_dir, worktree_branches, unborn })
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

    /// CI runners hand out 8.3 temp paths (`RUNNER~1`) while libgit2 reports long ones; compare like with like.
    fn long_path(p: &Path) -> std::path::PathBuf {
        dunce::canonicalize(p).unwrap()
    }

    #[test]
    fn plain_repo_from_subfolder() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = long_path(tmp.path()).join("proj");
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
        let main_dir = long_path(tmp.path()).join("main");
        let wt_dir = long_path(tmp.path()).join("wt");
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
    fn a_worktree_inside_the_main_checkout_is_told_apart_from_it() {
        let tmp = tempfile::tempdir().unwrap();
        let main_dir = long_path(tmp.path()).join("main");
        let wt_dir = main_dir.join(".claude/worktrees/fix");
        fs::create_dir_all(wt_dir.parent().unwrap()).unwrap();
        let repo = init_with_commit(&main_dir);
        repo.worktree("fix", &wt_dir, None).unwrap();
        fs::create_dir_all(wt_dir.join("src")).unwrap();

        let info = detect(&main_dir).unwrap();
        assert_eq!(path_key(info.main_checkout()), path_key(&main_dir));
        let folder = info.folder_of(&wt_dir.join("src")).unwrap();
        assert_eq!(path_key(folder), path_key(&wt_dir), "the deepest working tree wins");
        assert!(!info.is_main(folder));
        assert_eq!(info.branch_at(&wt_dir.join("src")), Some("fix"));
        assert_eq!(path_key(info.folder_of(&main_dir.join("src")).unwrap()), path_key(&main_dir));
        assert_eq!(info.branch_at(&main_dir), info.branch.as_deref());

        let from_wt = detect(&wt_dir).unwrap();
        assert_eq!(path_key(from_wt.main_checkout()), path_key(&main_dir));
        assert!(!from_wt.is_main(&from_wt.root));
    }

    #[test]
    fn a_repository_without_commits_is_unborn() {
        let tmp = tempfile::tempdir().unwrap();
        Repository::init(tmp.path()).unwrap();
        assert!(detect(tmp.path()).unwrap().unborn);

        let with_commit = tempfile::tempdir().unwrap();
        init_with_commit(with_commit.path());
        assert!(!detect(with_commit.path()).unwrap().unborn);
    }

    #[test]
    fn state_saved_before_worktree_sessions_still_loads() {
        let old = r#"{"root":"/p","branch":"main","worktrees":["/p-wt"],"is_worktree":false}"#;
        let info: GitInfo = serde_json::from_str(old).unwrap();
        assert_eq!(info.main_checkout(), Path::new("/p"));
        assert!(!info.unborn && info.worktree_branches.is_empty());
    }

    #[test]
    fn not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        // tempdir lives under the user's temp folder, which is not inside a repository.
        assert!(detect(tmp.path()).is_none());
    }
}
