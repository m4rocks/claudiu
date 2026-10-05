//! "Commit & Push": stage everything, have Haiku write the message through the user's own `claude` CLI
//! (`claude -p`, no tools, no session saved), commit, then push. "Pull" is a plain `git pull`; the branch menu
//! switches branches and creates new ones from origin (which `background_fetch` keeps fresh). The `git` CLI is
//! used rather than libgit2 so the user's hooks, signing, identity, credentials and pull config apply exactly
//! as for a manual commit.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

pub const MODEL: &str = "claude-haiku-4-5-20251001";
/// Keeps the prompt small and cheap; the stat summary always covers the whole change.
const MAX_DIFF_BYTES: usize = 40_000;

const SYSTEM: &str = "You write git commit messages. Reply with the commit message only: no preamble, no code \
fences, no quotes. First line: imperative summary, at most 72 characters. If the change warrants it, add a blank \
line and a few short lines of explanation. Match the style of the recent commits when they are shown.";

/// Stage all changes, commit them with a generated message (when there are any), then push.
/// Returns a one-line summary for a toast. Blocking.
pub fn commit_and_push(claude: &Path, cwd: &Path) -> Result<String, String> {
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
        let message = generate(claude, &prompt(&recent, &diff)).map_err(|e| format!("{e} (changes are staged)"))?;
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

fn has_origin(cwd: &Path) -> bool {
    git(cwd, &["remote", "get-url", "origin"], None).is_ok()
}

/// Branches for the branch menu, most recently committed first: origin's (as of the last background fetch)
/// plus local ones. Returns `(branches, current)`. Blocking.
pub fn branches(cwd: &Path) -> Result<(Vec<String>, Option<String>), String> {
    let refs = git(
        cwd,
        &["for-each-ref", "--sort=-committerdate", "--format=%(refname)", "refs/heads", "refs/remotes/origin"],
        None,
    )?;
    let mut seen = std::collections::HashSet::new();
    let names = refs
        .lines()
        .filter_map(|r| r.strip_prefix("refs/heads/").or_else(|| r.strip_prefix("refs/remotes/origin/")))
        .filter(|n| *n != "HEAD" && seen.insert(n.to_string()))
        .map(str::to_string)
        .collect();
    let current = git(cwd, &["branch", "--show-current"], None).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Ok((names, current))
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
fn origin_default_branch(cwd: &Path) -> Result<String, String> {
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

fn generate(claude: &Path, prompt: &str) -> Result<String, String> {
    let mut cmd = crate::claude::command(claude);
    cmd.args(["-p", "--model", MODEL, "--no-session-persistence", "--disable-slash-commands", "--strict-mcp-config"])
        // No tools, and none of the user's settings/hooks: this is a plain text-in, text-out call.
        .args(["--tools", "", "--setting-sources", "", "--system-prompt", SYSTEM])
        .env("CLAUDE_CODE_AUTO_CONNECT_IDE", "false")
        // Neutral folder: keeps project CLAUDE.md files out of the call.
        .current_dir(crate::platform::home_dir().unwrap_or_else(std::env::temp_dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("could not run claude: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    if child.wait_timeout(Duration::from_secs(120)).map_err(|e| e.to_string())?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("claude timed out".into());
    }
    let mut bytes = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut bytes);
    }
    clean_message(&String::from_utf8_lossy(&bytes)).ok_or_else(|| "claude returned no commit message".into())
}

/// Trim, and unwrap a code fence if the model added one anyway.
fn clean_message(raw: &str) -> Option<String> {
    let mut t = raw.trim();
    if let Some(rest) = t.strip_prefix("```") {
        t = rest.split_once('\n').map_or("", |(_, body)| body).trim_end_matches("```").trim();
    }
    (!t.is_empty()).then(|| t.to_string())
}

fn git(cwd: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
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
        let (names, current) = branches(&b).unwrap();
        let default = current.unwrap();
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
        assert_eq!(branches(&c).unwrap().1.as_deref(), Some("x"));
    }

    #[test]
    fn push_sets_upstream_and_pull_fetches() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, a, b) = (tmp.path(), tmp.path().join("a"), tmp.path().join("b"));
        git(root, &["init", "-q", "--bare", "remote.git"], None).unwrap();
        git(root, &["clone", "-q", "remote.git", "a"], None).unwrap();
        commit(&a, "one.txt");
        // Nothing staged (so no claude call) and no upstream yet: the existing commit is still pushed.
        assert_eq!(commit_and_push(Path::new("claude-not-needed"), &a).unwrap(), "Nothing to commit; pushed");
        git(root, &["clone", "-q", "remote.git", "b"], None).unwrap();
        assert!(b.join("one.txt").exists());
        assert_eq!(pull(&b).unwrap(), "Already up to date");
        commit(&a, "two.txt");
        commit_and_push(Path::new("claude-not-needed"), &a).unwrap();
        assert_eq!(pull(&b).unwrap(), "Pulled");
        assert!(b.join("two.txt").exists());
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
        let rejected = commit_and_push(Path::new("claude-not-needed"), &b).unwrap_err();
        assert!(classify(&b, rejected).conflict_in.is_some());
        // Identity is set because CI runners have none, and git refuses to merge without one.
        let failed = git(&b, &["-c", "pull.rebase=false", "-c", "user.name=t", "-c", "user.email=t@t", "pull"], None).unwrap_err();
        assert!(classify(&b, failed).conflict_in.is_some());
        assert_eq!(commit_and_push(Path::new("claude-not-needed"), &b).unwrap_err(), "Resolve the merge conflicts first");
        assert!(classify(&b, "git push failed: unable to access".into()).conflict_in.is_some(), "unmerged paths count");

        let clean = tmp.path().join("clean");
        git(root, &["clone", "-q", "remote.git", "clean"], None).unwrap();
        assert!(classify(&clean, "git pull failed: could not resolve host".into()).conflict_in.is_none());
    }
}
