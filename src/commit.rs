//! "Commit changes": stage everything, have Haiku write the message through the user's own `claude` CLI
//! (`claude -p`, no tools, no session saved), then commit. The `git` CLI is used rather than libgit2 so the
//! user's hooks, signing and identity config apply exactly as for a manual commit.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

pub const MODEL: &str = "claude-haiku-4-5-20251001";
/// Keeps the prompt small and cheap; the stat summary always covers the whole change.
const MAX_DIFF_BYTES: usize = 40_000;

const SYSTEM: &str = "You write git commit messages. Reply with the commit message only: no preamble, no code \
fences, no quotes. First line: imperative summary, at most 72 characters. If the change warrants it, add a blank \
line and a few short lines of explanation. Match the style of the recent commits when they are shown.";

/// Stage all changes, generate a message, commit. Returns the commit's subject line. Blocking.
pub fn run(claude: &Path, cwd: &Path) -> Result<String, String> {
    git(cwd, &["add", "-A"], None)?;
    let diff = git(cwd, &["diff", "--cached", "--stat", "--patch"], None)?;
    if diff.trim().is_empty() {
        return Err("Nothing to commit".into());
    }
    let recent = git(cwd, &["log", "-8", "--format=%s"], None).unwrap_or_default();
    let message = generate(claude, &prompt(&recent, &diff)).map_err(|e| format!("{e} (changes are staged)"))?;
    git(cwd, &["commit", "-F", "-"], Some(&message))?;
    Ok(message.lines().next().unwrap_or_default().to_string())
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

    #[test]
    fn nothing_to_commit_is_reported_and_changes_get_committed() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        git(d, &["init", "-q"], None).unwrap();
        assert_eq!(run(Path::new("claude-not-needed"), d).unwrap_err(), "Nothing to commit");
        std::fs::write(d.join("a.txt"), "hi").unwrap();
        git(d, &["add", "-A"], None).unwrap();
        git(d, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-F", "-"], Some("first\n\nbody")).unwrap();
        assert_eq!(git(d, &["log", "-1", "--format=%s"], None).unwrap().trim(), "first");
    }
}
