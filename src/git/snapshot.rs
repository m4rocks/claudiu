//! Per-turn snapshots: before each message a session sends, the whole working tree (untracked files too, ignored ones
//! not) is saved as a commit under `refs/m4code/<session>/`. The review compares the latest one with the working tree,
//! and a hunk turned down there is put back as it was in the snapshot.
//!
//! The commit is built through a temporary index file, so the user's index, HEAD, branches and stash are untouched,
//! and only git's plumbing runs: no hooks, no signing.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::commit::git_with;

/// Where a session's snapshots live.
fn prefix(session: &str) -> String {
    format!("refs/m4code/{}", crate::datadir::safe_file_stem(session))
}

/// Save the working tree of the repository at `cwd` as a snapshot of `session`, and return the commit's id. The whole
/// repository is saved, even when `cwd` is a folder inside it.
pub fn take(cwd: &Path, session: &str) -> Result<String, String> {
    let root = root(cwd)?;
    let cwd = root.as_path();
    let index = TempIndex::new()?;
    let env: &[(&str, &OsStr)] = &[
        ("GIT_INDEX_FILE", index.path()),
        ("GIT_AUTHOR_NAME", OsStr::new("m4code")),
        ("GIT_AUTHOR_EMAIL", OsStr::new("m4code@localhost")),
        ("GIT_COMMITTER_NAME", OsStr::new("m4code")),
        ("GIT_COMMITTER_EMAIL", OsStr::new("m4code@localhost")),
    ];
    let run = |args: &[&str]| git_with(cwd, args, None, env);
    let head = run(&["rev-parse", "--verify", "-q", "HEAD"]).ok().map(|h| h.trim().to_string());
    // Starting from HEAD's tree makes `add` hash only what changed since.
    if let Some(head) = &head {
        run(&["read-tree", head])?;
    }
    run(&["add", "-A", "."])?;
    let tree = run(&["write-tree"])?.trim().to_string();
    let mut args = vec!["commit-tree", &tree, "-m", "m4code snapshot"];
    if let Some(head) = &head {
        args.extend(["-p", head]);
    }
    let commit = run(&args)?.trim().to_string();
    let name = format!("{}/{}", prefix(session), now_millis());
    run(&["update-ref", &name, &commit])?;
    forget_old(cwd, session);
    Ok(commit)
}

/// Snapshots a session keeps; older ones are let go, so git can collect what only they held.
const KEPT: usize = 20;

fn forget_old(root: &Path, session: &str) {
    let Ok(refs) = git_with(root, &["for-each-ref", "--sort=-refname", "--format=%(refname)", &prefix(session)], None, &[]) else { return };
    for name in refs.lines().skip(KEPT) {
        let _ = git_with(root, &["update-ref", "-d", name], None, &[]);
    }
}

/// A path for a throwaway index, removed when dropped. Git wants the file not to exist yet: an empty file is a broken
/// index to it.
struct TempIndex(PathBuf);

impl TempIndex {
    fn new() -> Result<Self, String> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self(std::env::temp_dir().join(format!("m4code-index-{}-{n}", std::process::id()))))
    }

    fn path(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // Git leaves a lock file behind when it is stopped midway.
        let _ = std::fs::remove_file(self.0.with_extension("lock"));
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// The working tree root of the repository `cwd` is in.
fn root(cwd: &Path) -> Result<PathBuf, String> {
    Ok(PathBuf::from(git_with(cwd, &["rev-parse", "--show-toplevel"], None, &[])?.trim()))
}

/// One changed file between a snapshot and the working tree.
#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    /// Relative to the repository root.
    pub path: PathBuf,
    pub hunks: Vec<Hunk>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
    /// This hunk alone as a patch from the snapshot to the working tree, to apply in reverse when it is turned down.
    pub patch: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// `'+'`, `'-'` or `' '`.
    pub origin: char,
    pub text: String,
    /// The line's number in the working tree; none for a removed line.
    pub new_line: Option<u32>,
}

/// What changed in the working tree since `snapshot`, file by file. Binary files are left out: they have no lines.
pub fn changes(cwd: &Path, snapshot: &str) -> Result<Vec<FileChange>, String> {
    capture(cwd, snapshot).map(|patch| parse(&patch))
}

pub fn capture(cwd: &Path, snapshot: &str) -> Result<String, String> {
    let root = root(cwd)?;
    // Untracked files only show in a diff once git knows of them; `--intent-to-add` in a throwaway index does that
    // without staging anything the user can see.
    let index = TempIndex::new()?;
    let env: &[(&str, &OsStr)] = &[("GIT_INDEX_FILE", index.path())];
    let root = root.as_path();
    git_with(root, &["read-tree", snapshot], None, env)?;
    git_with(root, &["add", "-A", "--intent-to-add", "."], None, env)?;
    git_with(root, &["-c", "core.quotePath=false", "diff", "--no-color", "--no-ext-diff", "--no-renames", "--src-prefix=a/", "--dst-prefix=b/", "--binary", "-U3", snapshot], None, env)
}

/// A `git diff` split into files and hunks, each hunk carrying a patch of its own.
pub fn parse(patch: &str) -> Vec<FileChange> {
    let mut files: Vec<FileChange> = Vec::new();
    // The current file's header (`diff --git`, `---`, `+++` and the like), repeated in front of each of its hunks.
    let mut header = String::new();
    let (mut old, mut new) = (None, None);
    let mut in_header = false;
    let mut new_line = 0u32;
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            if in_header && let Some(path) = new.take().or_else(|| old.take()) {
                files.push(FileChange { path: PathBuf::from(path), hunks: Vec::new() });
            }
            (header, old, new, in_header) = (String::new(), None, None, true);
        }
        if in_header && !line.starts_with("@@") {
            header.push_str(line);
            header.push('\n');
            // Git ends the path with a tab when it has a space.
            if let Some(path) = line.strip_prefix("--- a/") {
                old = Some(path.trim_end_matches('\t').to_string());
            } else if let Some(path) = line.strip_prefix("+++ b/") {
                new = Some(path.trim_end_matches('\t').to_string());
            } else if let Some(paths) = line.strip_prefix("diff --git a/")
                && let Some((path, _)) = paths.rsplit_once(" b/")
            {
                old = Some(path.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@") {
            if in_header {
                in_header = false;
                // A deleted file has only its old path.
                let Some(path) = new.clone().or_else(|| old.clone()) else { continue };
                files.push(FileChange { path: PathBuf::from(path), hunks: Vec::new() });
            }
            new_line = rest.split_whitespace().find_map(|w| w.strip_prefix('+')).and_then(|w| w.split(',').next()).and_then(|n| n.parse().ok()).unwrap_or(1);
            let Some(file) = files.last_mut() else { continue };
            file.hunks.push(Hunk { header: line.to_string(), lines: Vec::new(), patch: format!("{header}{line}\n") });
            continue;
        }
        let Some(hunk) = files.last_mut().and_then(|f| f.hunks.last_mut()) else { continue };
        hunk.patch.push_str(line);
        hunk.patch.push('\n');
        // "\ No newline at end of file" belongs in the patch but isn't a line of the file.
        let Some(origin) = line.chars().next().filter(|c| matches!(c, '+' | '-' | ' ')) else { continue };
        let number = (origin != '-').then_some(new_line);
        if origin != '-' {
            new_line += 1;
        }
        hunk.lines.push(Line { origin, text: line[1..].to_string(), new_line: number });
    }
    if in_header && let Some(path) = new.or(old) {
        files.push(FileChange { path: PathBuf::from(path), hunks: Vec::new() });
    }
    files
}

fn checkpoint_ref(record: &str, id: uuid::Uuid) -> String {
    format!("refs/m4code-checkpoints/{}/{id}", crate::datadir::safe_file_stem(record))
}

pub fn keep_checkpoint(cwd: &Path, record: &str, id: uuid::Uuid, before: &str) -> Result<(), String> {
    git_with(cwd, &["update-ref", &checkpoint_ref(record, id), before], None, &[]).map(|_| ())
}

pub fn forget_checkpoint(cwd: &Path, record: &str, id: uuid::Uuid) {
    let _ = git_with(cwd, &["update-ref", "-d", &checkpoint_ref(record, id)], None, &[]);
}

/// Restore a turn as a single patch, only when the checkout still matches its captured end state.
pub fn restore(cwd: &Path, before: &str, patch: &str) -> Result<(), String> {
    if capture(cwd, before)? != patch {
        return Err("The checkout has changed since that turn.".into());
    }
    git_with(&root(cwd)?, &["apply", "--reverse", "--whitespace=nowarn", "-"], Some(patch), &[]).map(|_| ())
}

/// Put one hunk back as it was in the snapshot. Fails, leaving the file alone, when the file has changed since the
/// hunk was read.
pub fn revert(cwd: &Path, hunk: &Hunk) -> Result<(), String> {
    git_with(&root(cwd)?, &["apply", "--reverse", "--whitespace=nowarn", "-"], Some(&hunk.patch), &[]).map(|_| ())
}

/// Re-apply a previously reversed turn patch forward (used to roll back a partial multi-turn restore).
pub fn apply_forward(cwd: &Path, patch: &str) -> Result<(), String> {
    git_with(&root(cwd)?, &["apply", "--whitespace=nowarn", "-"], Some(patch), &[]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // Without `core.autocrlf false`, git on Windows (where it is usually on) writes the files these tests
        // compare line by line back with CRLF endings.
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["config", "core.autocrlf", "false"]] {
            assert!(std::process::Command::new("git").args(args).current_dir(dir.path()).status().unwrap().success());
        }
        dir
    }

    fn commit_all(dir: &Path) {
        for args in [&["add", "-A"][..], &["commit", "-q", "-m", "c"]] {
            assert!(std::process::Command::new("git").args(args).current_dir(dir).status().unwrap().success());
        }
    }

    #[test]
    fn a_snapshot_leaves_the_users_index_and_head_alone() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        commit_all(dir.path());
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();
        let status = || String::from_utf8(std::process::Command::new("git").args(["status", "--porcelain"]).current_dir(dir.path()).output().unwrap().stdout).unwrap();
        let before = status();
        let snap = take(dir.path(), "s1").unwrap();
        assert_eq!(status(), before, "nothing got staged");
        assert!(changes(dir.path(), &snap).unwrap().is_empty(), "the snapshot holds the working tree, new files too");
        let refs = String::from_utf8(std::process::Command::new("git").args(["for-each-ref", "refs/m4code/s1"]).current_dir(dir.path()).output().unwrap().stdout).unwrap();
        assert!(refs.contains(&snap), "the snapshot is kept under the session's refs: {refs}");
    }

    #[test]
    fn a_session_keeps_only_its_newest_snapshots() {
        let dir = repo();
        let snaps: Vec<String> = (0..KEPT + 2)
            .map(|n| {
                std::fs::write(dir.path().join("a.txt"), n.to_string()).unwrap();
                // Snapshot names are milliseconds; two in the same one would share a name.
                std::thread::sleep(std::time::Duration::from_millis(2));
                take(dir.path(), "s").unwrap()
            })
            .collect();
        let refs = String::from_utf8(std::process::Command::new("git").args(["for-each-ref", "--format=%(objectname)", "refs/m4code/s"]).current_dir(dir.path()).output().unwrap().stdout).unwrap();
        assert_eq!(refs.lines().count(), KEPT);
        assert!(!refs.contains(&snaps[0]) && !refs.contains(&snaps[1]) && refs.contains(snaps.last().unwrap()), "the oldest go first");
    }

    #[test]
    fn a_snapshot_works_before_the_first_commit() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let snap = take(dir.path(), "s").unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let files = changes(dir.path(), &snap).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn changes_since_a_snapshot_come_hunk_by_hunk_and_a_hunk_reverts_alone() {
        let dir = repo();
        let lines: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("a.txt"), &lines).unwrap();
        commit_all(dir.path());
        let snap = take(dir.path(), "s").unwrap();
        let edited = lines.replace("line 2\n", "line two\n").replace("line 18\n", "line eighteen\n");
        std::fs::write(dir.path().join("a.txt"), &edited).unwrap();
        std::fs::write(dir.path().join("new file.txt"), "hello\n").unwrap();

        let files = changes(dir.path(), &snap).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, ["a.txt", "new file.txt"]);
        let a = &files[0];
        assert_eq!(a.hunks.len(), 2);
        let added = a.hunks[0].lines.iter().find(|l| l.origin == '+').unwrap();
        assert_eq!((added.text.as_str(), added.new_line), ("line two", Some(2)));

        revert(dir.path(), &a.hunks[1]).unwrap();
        let now = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert!(now.contains("line two\n") && now.contains("line 18\n"), "only the second hunk went back: {now}");

        revert(dir.path(), &files[1].hunks[0]).unwrap();
        assert!(!dir.path().join("new file.txt").exists(), "turning down a new file removes it");
    }

    #[test]
    fn checkpoint_restore_rejects_later_edits_and_restores_new_binary_files() {
        let dir = repo();
        let text = dir.path().join("note.txt");
        let binary = dir.path().join("new.bin");
        std::fs::write(&text, "before\n").unwrap();
        commit_all(dir.path());
        let before = take(dir.path(), "s").unwrap();
        std::fs::write(&text, "after\n").unwrap();
        std::fs::write(&binary, [0, 1, 2, 3]).unwrap();
        let patch = capture(dir.path(), &before).unwrap();
        assert_eq!(parse(&patch).iter().map(|file| file.path.clone()).collect::<Vec<_>>(),
            [PathBuf::from("new.bin"), PathBuf::from("note.txt")]);
        std::fs::write(&text, "later\n").unwrap();
        assert!(restore(dir.path(), &before, &patch).is_err());
        assert!(binary.exists());
        assert_eq!(std::fs::read_to_string(&text).unwrap(), "later\n");
        std::fs::write(&text, "after\n").unwrap();
        restore(dir.path(), &before, &patch).unwrap();
        assert!(!binary.exists());
        assert_eq!(std::fs::read_to_string(text).unwrap(), "before\n");
    }

    #[test]
    fn a_hunk_that_no_longer_matches_is_not_applied() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        commit_all(dir.path());
        let snap = take(dir.path(), "s").unwrap();
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        let hunk = changes(dir.path(), &snap).unwrap()[0].hunks[0].clone();
        std::fs::write(dir.path().join("a.txt"), "three\n").unwrap();
        assert!(revert(dir.path(), &hunk).is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "three\n");
    }
}
