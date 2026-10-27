//! "Commit & Push": stage everything, have Haiku write the message through the running session's own mod
//! (`claude/ask.rs`; no `claude -p`, no second process), commit, then push. "Pull" is a plain `git pull`; the branch menu
//! switches branches and creates new ones from origin (which `background_fetch` keeps fresh). The `git` CLI is
//! used rather than libgit2 so the user's hooks, signing, identity, credentials and pull config apply exactly
//! as for a manual commit.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A text-in, text-out completion: `(system, prompt)` to the model's reply. In the app it is `ask::complete`.
pub type Complete<'a> = &'a dyn Fn(&str, &str) -> Result<String, String>;

/// Keeps the prompt small and cheap; the stat summary always covers the whole change.
const MAX_DIFF_BYTES: usize = 40_000;

const SYSTEM: &str = "You write git commit messages. Reply with the commit message only: no preamble, no code \
fences, no quotes. First line: imperative summary, at most 72 characters. If the change warrants it, add a blank \
line and a few short lines of explanation. Match the style of the recent commits when they are shown.";

/// Stage all changes, commit them with a generated message (when there are any), then push.
/// Returns a one-line summary for a toast. Blocking.
pub fn commit_and_push(complete: Complete, cwd: &Path) -> Result<String, String> {
    // Staging now would commit the conflict markers.
    if has_unmerged(cwd) {
        return Err("Resolve the merge conflicts first".into());
    }
    git(cwd, &["add", "-A"], None)?;
    let diff = git(cwd, &["diff", "--cached", "--stat", "--patch"], None)?;
    let subject = if diff.trim().is_empty() {
        None
    } else {
        let recent = git(cwd, &["log", "-8", "--format=%s"], None).unwrap_or_default();
        let message = generate(complete, SYSTEM, &prompt(&recent, &diff)).map_err(|e| format!("{e} (changes are staged)"))?;
        git(cwd, &["commit", "-F", "-"], Some(&message))?;
        Some(message.lines().next().unwrap_or_default().to_string())
    };
    // A branch without an upstream gets one on its first push.
    let has_upstream = git(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], None).is_ok();
    let push: &[&str] = if has_upstream { &["push"] } else { &["push", "-u", "origin", "HEAD"] };
    match (git(cwd, push, None), subject) {
        (Ok(_), Some(s)) => Ok(format!("Committed and pushed: {s}")),
        (Ok(_), None) => Ok("Nothing to commit; pushed".into()),
        (Err(e), Some(s)) => Err(format!("Committed “{s}”, but {e}")),
        (Err(e), None) => Err(e),
    }
}

/// What Commit & Push would have to do in a folder.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pending {
    /// Changed or new files in the working tree.
    pub changes: bool,
    /// Commits origin hasn't got, or a branch origin doesn't have yet.
    pub unpushed: bool,
}

impl Pending {
    pub fn any(self) -> bool {
        self.changes || self.unpushed
    }
}

/// Blocking. Runs every few seconds beside Claude's own git commands, so it must not take `index.lock` to refresh the index.
pub fn pending(cwd: &Path) -> Result<Pending, String> {
    let changes = !git(cwd, &["--no-optional-locks", "status", "--porcelain"], None)?.trim().is_empty();
    let unpushed = match git(cwd, &["rev-list", "--count", "@{u}..HEAD"], None) {
        Ok(count) => count.trim() != "0",
        // No upstream: the first push publishes the branch, if there is an origin and a commit to publish.
        Err(_) => has_origin(cwd) && git(cwd, &["rev-parse", "--verify", "-q", "HEAD"], None).is_ok(),
    };
    Ok(Pending { changes, unpushed })
}

/// `git pull` with the user's own pull config (merge / rebase / ff-only). Blocking.
pub fn pull(cwd: &Path) -> Result<String, String> {
    let out = git(cwd, &["pull"], None)?;
    Ok(if out.contains("Already up to date") { "Already up to date".into() } else { "Pulled".into() })
}

/// A failed git operation; `conflict_in` is set when it stopped on a merge/rebase conflict or a rejected push,
/// i.e. something Claude can sort out.
pub struct GitFail {
    pub msg: String,
    pub conflict_in: Option<PathBuf>,
}

impl From<String> for GitFail {
    fn from(msg: String) -> Self {
        Self { msg, conflict_in: None }
    }
}

/// Tag a pull / commit & push failure as a conflict when the repository has unmerged paths or git says the
/// branches diverged / the push was rejected. Blocking.
pub fn classify(cwd: &Path, msg: String) -> GitFail {
    let rejected = ["[rejected]", "non-fast-forward", "fetch first", "divergent branches", "CONFLICT", "Automatic merge failed"]
        .iter()
        .any(|m| msg.contains(m));
    let conflict = rejected || has_unmerged(cwd);
    GitFail { msg, conflict_in: conflict.then(|| cwd.to_path_buf()) }
}

fn has_unmerged(cwd: &Path) -> bool {
    git(cwd, &["diff", "--name-only", "--diff-filter=U"], None).is_ok_and(|o| !o.trim().is_empty())
}

/// Fetch origin quietly so branch lists are fresh before the menu is opened. Blocking.
pub fn background_fetch(cwd: &Path) {
    if has_origin(cwd) {
        let _ = git(cwd, &["fetch", "--prune", "--quiet", "origin"], None);
    }
}

pub(super) fn has_origin(cwd: &Path) -> bool {
    git(cwd, &["remote", "get-url", "origin"], None).is_ok()
}

/// A branch that a working tree has checked out.
#[derive(Debug, PartialEq)]
pub struct Held {
    pub branch: String,
    /// "main checkout" for the first working tree git lists, else the folder's name.
    pub tree: String,
    pub folder: PathBuf,
}

/// What the branch menu shows.
#[derive(Debug, Default)]
pub struct BranchList {
    /// Origin's branches (as of the last background fetch) plus local ones, most recently committed first.
    pub names: Vec<String>,
    pub current: Option<String>,
    /// Branches another working tree has checked out, with that tree's name. Git refuses to switch to these.
    pub elsewhere: Vec<Held>,
}

/// Branches for the branch menu. Blocking.
pub fn branches(cwd: &Path) -> Result<BranchList, String> {
    let refs = git(
        cwd,
        &["for-each-ref", "--sort=-committerdate", "--format=%(refname)", "refs/heads", "refs/remotes/origin"],
        None,
    )?;
    let current = git(cwd, &["branch", "--show-current"], None).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let worktrees = git(cwd, &["worktree", "list", "--porcelain"], None).unwrap_or_default();
    let elsewhere = checked_out(&worktrees).into_iter().filter(|h| current.as_ref() != Some(&h.branch)).collect();
    let mut seen = std::collections::HashSet::new();
    let names = refs
        .lines()
        .filter_map(|r| r.strip_prefix("refs/heads/").or_else(|| r.strip_prefix("refs/remotes/origin/")))
        .filter(|n| *n != "HEAD" && seen.insert(n.to_string()))
        .map(str::to_string)
        .collect();
    Ok(BranchList { names, current, elsewhere })
}

/// The local branches the branch menu may delete: not the current one, and not origin's default branch.
pub fn deletable_branches(cwd: &Path, current: Option<&str>) -> Vec<String> {
    let default = git(cwd, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], None).ok().map(|s| s.trim().trim_start_matches("origin/").to_string());
    let local = git(cwd, &["for-each-ref", "--format=%(refname:short)", "refs/heads"], None).unwrap_or_default();
    local.lines().filter(|b| Some(*b) != current && Some(*b) != default.as_deref()).map(str::to_string).collect()
}

/// How many commits are on this branch and on no other local branch and no remote branch. Blocking.
pub fn commits_only_on(cwd: &Path, branch: &str) -> Result<usize, String> {
    // The glob after --exclude is relative to refs/heads, so origin/<branch> still counts as another place for the commits.
    let count = git(cwd, &["rev-list", "--count", branch, "--not", &format!("--exclude={branch}"), "--branches", "--remotes"], None)?;
    count.trim().parse().map_err(|_| format!("Could not count the commits on {branch}"))
}

/// Delete a local branch, whatever it holds. The caller has shown what would be lost. Blocking.
pub fn delete_branch(cwd: &Path, branch: &str) -> Result<String, String> {
    git(cwd, &["branch", "-D", "--", branch], None)?;
    Ok(format!("Deleted branch {branch}"))
}

/// The branches that `git worktree list --porcelain` shows as checked out, each with the name of its working tree:
/// "main checkout" for the first one listed, else the folder's name.
fn checked_out(porcelain: &str) -> Vec<Held> {
    let mut found = vec![];
    for (i, block) in porcelain.split("

").enumerate() {
        let path = block.lines().find_map(|l| l.strip_prefix("worktree "));
        let branch = block.lines().find_map(|l| l.strip_prefix("branch refs/heads/"));
        if let (Some(path), Some(branch)) = (path, branch) {
            let name = if i == 0 { "main checkout".to_string() } else { path.rsplit(['/', '\\']).next().unwrap_or(path).to_string() };
            found.push(Held { branch: branch.to_string(), tree: name, folder: PathBuf::from(path) });
        }
    }
    found
}

/// Switch branch; a branch that only exists on origin gets a local tracking branch. Blocking.
pub fn switch_branch(cwd: &Path, name: &str) -> Result<String, String> {
    git(cwd, &["switch", name], None)?;
    Ok(format!("Switched to {name}"))
}

/// New branch from origin's default branch (fetched first), or from the current commit when there is no origin.
/// It has no upstream yet, so the first Commit & Push publishes it under the same name. Blocking.
pub fn create_branch(cwd: &Path, name: &str) -> Result<String, String> {
    if !has_origin(cwd) {
        git(cwd, &["switch", "-c", name], None)?;
        return Ok(format!("Created {name}"));
    }
    git(cwd, &["fetch", "origin"], None)?;
    let base = origin_default_branch(cwd)?;
    git(cwd, &["switch", "--no-track", "-c", name, &base], None)?;
    Ok(format!("Created {name} from {base}"))
}

/// `origin/<default>`: from the local `origin/HEAD` when set, else asked from the remote.
pub(super) fn origin_default_branch(cwd: &Path) -> Result<String, String> {
    if let Ok(r) = git(cwd, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], None) {
        return Ok(r.trim().to_string());
    }
    let out = git(cwd, &["ls-remote", "--symref", "origin", "HEAD"], None)?;
    out.lines()
        .find_map(|l| l.strip_prefix("ref: refs/heads/")?.split_once('\t').map(|(b, _)| format!("origin/{b}")))
        .ok_or_else(|| "could not find origin's default branch".into())
}

fn prompt(recent: &str, diff: &str) -> String {
    let mut end = diff.len().min(MAX_DIFF_BYTES);
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    let cut = if end < diff.len() { "\n[diff truncated]" } else { "" };
    format!("Recent commit subjects:\n{recent}\n\nStaged changes:\n{}{cut}\n\nWrite the commit message.", &diff[..end])
}

/// One text-in, text-out call; the reply is trimmed and unwrapped from a code fence. Blocking; also titles sessions.
pub fn generate(complete: Complete, system: &str, prompt: &str) -> Result<String, String> {
    clean_message(&complete(system, prompt)?).ok_or_else(|| "the model returned nothing".into())
}

/// Trim, and unwrap a code fence if the model added one anyway.
fn clean_message(raw: &str) -> Option<String> {
    let mut t = raw.trim();
    if let Some(rest) = t.strip_prefix("```") {
        t = rest.split_once('\n').map_or("", |(_, body)| body).trim_end_matches("```").trim();
    }
    (!t.is_empty()).then(|| t.to_string())
}

pub(super) fn git(cwd: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // No terminal can answer a credential prompt: fail instead of hanging (GUI credential helpers still work).
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    crate::platform::no_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("could not run git: {e}"))?;
    if let (Some(mut pipe), Some(text)) = (child.stdin.take(), stdin) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let out = String::from_utf8_lossy(&out.stdout);
        // Hook failures print to either stream.
        Err(format!("git {} failed: {}", args[0], if err.trim().is_empty() { out } else { err }.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_checked_out_in_worktrees_are_found() {
        let porcelain = "worktree /p
HEAD aaa
branch refs/heads/main

worktree /p/.claude/worktrees/x
HEAD bbb
branch refs/heads/feature/login
locked claude session x (pid 1)

worktree /p/.claude/worktrees/d
HEAD ccc
detached
";
        let held = |b: &str, w: &str, f: &str| Held { branch: b.into(), tree: w.into(), folder: f.into() };
        assert_eq!(checked_out(porcelain), [held("main", "main checkout", "/p"), held("feature/login", "x", "/p/.claude/worktrees/x")]);
        assert!(checked_out("").is_empty());
    }

    #[test]
    fn the_branch_menu_names_the_worktree_that_has_a_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let main = dunce::canonicalize(tmp.path()).unwrap().join("main");
        std::fs::create_dir_all(&main).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "one"], &["branch", "spare"]] {
            git(&main, args, None).unwrap();
        }
        let wt = tmp.path().join("wt");
        git(&main, &["worktree", "add", "-q", "-b", "in-worktree", &wt.to_string_lossy()], None).unwrap();

        let from_main = branches(&main).unwrap();
        assert_eq!(from_main.current.as_deref(), Some("main"));
        assert!(["spare", "main", "in-worktree"].iter().all(|b| from_main.names.contains(&b.to_string())), "{from_main:?}");
        assert_eq!((from_main.elsewhere[0].branch.as_str(), from_main.elsewhere[0].tree.as_str()), ("in-worktree", "wt"));
        assert_eq!(from_main.elsewhere.len(), 1);

        let from_wt = branches(&wt).unwrap();
        assert_eq!(from_wt.current.as_deref(), Some("in-worktree"));
        assert!(from_wt.names.contains(&"main".to_string()), "{from_wt:?}");
        assert_eq!((from_wt.elsewhere[0].branch.as_str(), from_wt.elsewhere[0].tree.as_str()), ("main", "main checkout"));
    }

    #[test]
    fn fences_are_unwrapped() {
        assert_eq!(clean_message("```\nFix bug\n\nbody\n```\n").as_deref(), Some("Fix bug\n\nbody"));
        assert_eq!(clean_message("  Add thing \n").as_deref(), Some("Add thing"));
        assert_eq!(clean_message("```\n```"), None);
    }

    #[test]
    fn prompt_truncates_on_a_char_boundary() {
        let diff = "é".repeat(MAX_DIFF_BYTES);
        assert!(prompt("", &diff).contains("[diff truncated]"));
    }

    fn no_model(_: &str, _: &str) -> Result<String, String> {
        Err("no model call was expected".into())
    }

    #[test]
    fn the_message_comes_from_the_completion_and_is_committed() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, a) = (tmp.path(), tmp.path().join("a"));
        git(root, &["init", "-q", "--bare", "remote.git"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "a"], None).unwrap();
        git(&a, &["config", "user.name", "t"], None).unwrap();
        git(&a, &["config", "user.email", "t@t"], None).unwrap();
        std::fs::write(a.join("new.txt"), "x").unwrap();
        let said = std::cell::RefCell::new(String::new());
        let model = |system: &str, prompt: &str| {
            said.replace(format!("{system}|{prompt}"));
            Ok("```\nAdd new.txt\n\nwhy\n```".to_string())
        };
        assert_eq!(commit_and_push(&model, &a).unwrap(), "Committed and pushed: Add new.txt");
        assert!(said.borrow().contains("new.txt"), "the diff goes to the model");
        assert_eq!(git(&a, &["log", "-1", "--format=%B"], None).unwrap().trim(), "Add new.txt\n\nwhy");
        // A failed completion leaves the changes staged and says so.
        std::fs::write(a.join("two.txt"), "y").unwrap();
        let err = commit_and_push(&|_: &str, _: &str| Err("the session didn't pick the request up".to_string()), &a).unwrap_err();
        assert!(err.ends_with("(changes are staged)"), "{err}");
    }

    fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).unwrap();
        git(dir, &["add", "-A"], None).unwrap();
        git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-F", "-"], Some(file)).unwrap();
    }

    #[test]
    fn branches_switch_and_new_branches_start_from_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, a, b, c) = (tmp.path(), tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("c"));
        git(root, &["init", "-q", "--bare", "remote.git"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "a"], None).unwrap();
        commit(&a, "one.txt");
        git(&a, &["push", "-q", "origin", "HEAD"], None).unwrap();
        git(&a, &["switch", "-q", "-c", "feature"], None).unwrap();
        commit(&a, "two.txt");
        git(&a, &["push", "-q", "origin", "feature"], None).unwrap();

        git(root, &["clone", "-q", "remote.git", "b"], None).unwrap();
        background_fetch(&b);
        let (names, default) = { let l = branches(&b).unwrap(); (l.names, l.current.unwrap()) };
        assert!(names.contains(&"feature".to_string()) && names.contains(&default), "{names:?}");
        switch_branch(&b, "feature").unwrap();
        assert!(b.join("two.txt").exists(), "origin-only branch is checked out");
        // Created from origin's default branch, not from the branch we are on.
        assert_eq!(create_branch(&b, "fresh").unwrap(), format!("Created fresh from origin/{default}"));
        assert!(!b.join("two.txt").exists());

        // No origin: from the current commit.
        git(root, &["init", "-q", "c"], None).unwrap();
        commit(&c, "c.txt");
        create_branch(&c, "x").unwrap();
        assert_eq!(branches(&c).unwrap().current.as_deref(), Some("x"));
    }

    #[test]
    fn a_branch_is_deletable_unless_current_or_default_and_reports_commits_nowhere_else() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, a) = (tmp.path(), tmp.path().join("a"));
        git(root, &["init", "-q", "--bare", "remote.git"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "a"], None).unwrap();
        commit(&a, "one.txt");
        git(&a, &["push", "-q", "-u", "origin", "HEAD"], None).unwrap();
        git(&a, &["remote", "set-head", "origin", "-a"], None).unwrap();
        let default = git(&a, &["branch", "--show-current"], None).unwrap().trim().to_string();

        git(&a, &["switch", "-q", "-c", "pushed"], None).unwrap();
        commit(&a, "two.txt");
        git(&a, &["push", "-q", "origin", "pushed"], None).unwrap();
        git(&a, &["switch", "-q", "-c", "local-only"], None).unwrap();
        commit(&a, "three.txt");
        commit(&a, "four.txt");
        git(&a, &["switch", "-q", &default], None).unwrap();
        git(&a, &["branch", "merged"], None).unwrap();

        let mut names = deletable_branches(&a, Some(&default));
        names.sort();
        assert_eq!(names, ["local-only", "merged", "pushed"], "not the current or default branch");
        assert!(!deletable_branches(&a, Some("pushed")).contains(&default), "origin's default is never offered");

        assert_eq!(commits_only_on(&a, "merged").unwrap(), 0, "the default branch has them");
        assert_eq!(commits_only_on(&a, "pushed").unwrap(), 0, "origin has them");
        assert_eq!(commits_only_on(&a, "local-only").unwrap(), 2);

        assert_eq!(delete_branch(&a, "local-only").unwrap(), "Deleted branch local-only");
        assert!(!deletable_branches(&a, Some(&default)).contains(&"local-only".to_string()));
    }

    #[test]
    fn push_sets_upstream_and_pull_fetches() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, a, b) = (tmp.path(), tmp.path().join("a"), tmp.path().join("b"));
        git(root, &["init", "-q", "--bare", "remote.git"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "a"], None).unwrap();
        commit(&a, "one.txt");
        // Nothing staged (so no claude call) and no upstream yet: the existing commit is still pushed.
        assert_eq!(commit_and_push(&no_model, &a).unwrap(), "Nothing to commit; pushed");
        git(root, &["clone", "-q", "remote.git", "b"], None).unwrap();
        assert!(b.join("one.txt").exists());
        assert_eq!(pull(&b).unwrap(), "Already up to date");
        commit(&a, "two.txt");
        commit_and_push(&no_model, &a).unwrap();
        assert_eq!(pull(&b).unwrap(), "Pulled");
        assert!(b.join("two.txt").exists());
    }

    #[test]
    fn pending_work_is_changes_or_commits_origin_lacks() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, a) = (tmp.path(), tmp.path().join("a"));
        git(root, &["init", "-q", "--bare", "remote.git"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "a"], None).unwrap();
        assert_eq!(pending(&a).unwrap(), Pending::default(), "an empty clone has nothing to do");
        commit(&a, "one.txt");
        assert_eq!(pending(&a).unwrap(), Pending { changes: false, unpushed: true }, "a branch origin doesn't have yet");
        commit_and_push(&no_model, &a).unwrap();
        assert!(!pending(&a).unwrap().any(), "pushed and clean");
        commit(&a, "two.txt");
        assert_eq!(pending(&a).unwrap(), Pending { changes: false, unpushed: true }, "a commit origin lacks");
        git(&a, &["push", "-q"], None).unwrap();
        std::fs::write(a.join("three.txt"), "x").unwrap();
        assert_eq!(pending(&a).unwrap(), Pending { changes: true, unpushed: false }, "a new file");
    }

    #[test]
    fn a_folder_without_origin_has_nothing_to_push() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "c"], None).unwrap();
        let c = tmp.path().join("c");
        commit(&c, "c.txt");
        assert!(!pending(&c).unwrap().any());
    }

    #[test]
    fn pull_conflict_is_classified_and_blocks_commit_and_push() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, a, b) = (tmp.path(), tmp.path().join("a"), tmp.path().join("b"));
        git(root, &["init", "-q", "--bare", "remote.git"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "a"], None).unwrap();
        commit(&a, "f.txt");
        git(&a, &["push", "-q", "origin", "HEAD"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "b"], None).unwrap();
        for (dir, text) in [(&a, "from a"), (&b, "from b")] {
            std::fs::write(dir.join("f.txt"), text).unwrap();
            git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "edit"], None).unwrap();
        }
        git(&a, &["push", "-q", "origin", "HEAD"], None).unwrap();

        // b's push is rejected (remote moved on), and a merge pull conflicts.
        let rejected = commit_and_push(&no_model, &b).unwrap_err();
        assert!(classify(&b, rejected).conflict_in.is_some());
        // Identity is set because CI runners have none, and git refuses to merge without one.
        let failed = git(&b, &["-c", "pull.rebase=false", "-c", "user.name=t", "-c", "user.email=t@t", "pull"], None).unwrap_err();
        assert!(classify(&b, failed).conflict_in.is_some());
        assert_eq!(commit_and_push(&no_model, &b).unwrap_err(), "Resolve the merge conflicts first");
        assert!(classify(&b, "git push failed: unable to access".into()).conflict_in.is_some(), "unmerged paths count");

        let clean = tmp.path().join("clean");
        git(root, &["clone", "-q", "remote.git", "clean"], None).unwrap();
        assert!(classify(&clean, "git pull failed: could not resolve host".into()).conflict_in.is_none());
    }
}
