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

/// One file's part of a unified `git diff`: the paths it reads and writes (`None` for `/dev/null`) and its text.
#[derive(Debug, PartialEq)]
struct FilePatch {
    old: Option<PathBuf>,
    new: Option<PathBuf>,
    text: String,
}

impl FilePatch {
    /// The file a restore puts back: where the turn left it.
    fn path(&self) -> &Path {
        self.new.as_deref().or(self.old.as_deref()).expect("parse_file_patches keeps only patches with a path")
    }
}

/// Read a path as git writes it in a diff header, plain or in C-style quotes, without its `prefix/` (`a/` or `b/`).
/// Returns it with the rest of `text` after a quoted path (nothing after a plain one, which runs to the end).
fn header_path(text: &str, prefix: char) -> Option<(String, &str)> {
    let Some(quoted) = text.strip_prefix('"') else {
        let path = text.strip_prefix(prefix)?.strip_prefix('/')?;
        return Some((path.to_string(), ""));
    };
    let bytes = quoted.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        match *bytes.get(i)? {
            b'"' => break,
            b'\\' => {
                let e = *bytes.get(i + 1)?;
                i += 2;
                match e {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'v' => out.push(11),
                    b'0'..=b'3' => {
                        let (d2, d3) = (*bytes.get(i)?, *bytes.get(i + 1)?);
                        if !(b'0'..=b'7').contains(&d2) || !(b'0'..=b'7').contains(&d3) {
                            return None;
                        }
                        i += 2;
                        out.push(((e - b'0') << 6) | ((d2 - b'0') << 3) | (d3 - b'0'));
                    }
                    other => out.push(other),
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    let path = String::from_utf8(out).ok()?;
    let path = path.strip_prefix(prefix)?.strip_prefix('/')?.to_string();
    Some((path, &quoted[i + 1..]))
}

/// The path in the value of a `---` or `+++` line: `None` for `/dev/null`.
fn side_path(value: &str, prefix: char) -> Option<Option<PathBuf>> {
    let value = value.trim_end_matches('\t');
    if value == "/dev/null" {
        return Some(None);
    }
    header_path(value, prefix).map(|(path, _)| Some(PathBuf::from(path)))
}

/// Split a unified `git diff` into one patch per file. Fails when a file's part names no path it can read, so a
/// restore never touches the wrong file or skips one.
fn parse_file_patches(patch: &str) -> Result<Vec<FilePatch>, String> {
    let mut out: Vec<FilePatch> = Vec::new();
    let mut in_header = false;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            in_header = true;
            // `diff --git a/x b/x` names the file even without `---`/`+++` lines (mode changes, binary files).
            let (old, new) = if rest.starts_with('"') {
                match header_path(rest, 'a') {
                    Some((old, after)) => (Some(old), header_path(after.trim_start(), 'b').map(|(new, _)| new)),
                    None => (None, None),
                }
            } else {
                match rest.strip_prefix("a/").and_then(|paths| paths.rsplit_once(" b/")) {
                    Some((old, new)) => (Some(old.to_string()), Some(new.to_string())),
                    None => (None, None),
                }
            };
            out.push(FilePatch { old: old.map(PathBuf::from), new: new.map(PathBuf::from), text: String::new() });
        } else if in_header && let Some(file) = out.last_mut() {
            if line.starts_with("@@") {
                in_header = false;
            } else if let Some(value) = line.strip_prefix("--- ") {
                file.old = side_path(value, 'a').ok_or_else(|| format!("Cannot read the path in `{line}`."))?;
            } else if let Some(value) = line.strip_prefix("+++ ") {
                file.new = side_path(value, 'b').ok_or_else(|| format!("Cannot read the path in `{line}`."))?;
            }
        }
        if let Some(file) = out.last_mut() {
            file.text.push_str(line);
            file.text.push('\n');
        }
    }
    if let Some(file) = out.iter().find(|f| f.old.is_none() && f.new.is_none()) {
        return Err(format!("Cannot tell which file this part of the checkpoint changes: {}", file.text.lines().next().unwrap_or_default()));
    }
    Ok(out)
}

/// The working-tree files some turn patches touch, as they were before a restore, so a failed restore can put every
/// one of them back (contents, or absence) instead of leaving some restored and some not.
pub struct Journal(Vec<(PathBuf, Option<Vec<u8>>)>);

impl Journal {
    /// Read every file `patch` (a turn's diff, relative to the repository of `cwd`) names, before anything changes.
    pub fn capture(cwd: &Path, patch: &str) -> Result<Journal, String> {
        let root = root(cwd)?;
        let mut files = Vec::new();
        for file in parse_file_patches(patch)? {
            for path in [file.old, file.new].into_iter().flatten() {
                let full = root.join(&path);
                let bytes = match std::fs::read(&full) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(format!("{}: {error}", path.display())),
                };
                files.push((full, bytes));
            }
        }
        Ok(Journal(files))
    }

    /// Put every file back as it was captured. Best effort: a file that cannot be written does not stop the rest.
    pub fn put_back(&self) {
        for (path, bytes) in &self.0 {
            match bytes {
                Some(bytes) => {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(path, bytes);
                }
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
}

/// Restore a single working-tree file directly from the `before` snapshot commit using a temporary index so the
/// user's own git index and HEAD are never modified. Removes the file if it did not exist in `before`.
fn restore_file_from_snapshot(root: &Path, before: &str, path: &Path) -> Result<(), String> {
    let git_path = path.to_string_lossy().replace('\\', "/");
    let spec = format!("{before}:{git_path}");
    if git_with(root, &["cat-file", "-e", &spec], None, &[]).is_ok() {
        let index = TempIndex::new()?;
        let env: &[(&str, &OsStr)] = &[("GIT_INDEX_FILE", index.path())];
        git_with(root, &["read-tree", before], None, env)?;
        git_with(root, &["checkout-index", "-f", "--", &git_path], None, env)?;
    } else {
        let full = root.join(path);
        if full.exists() {
            std::fs::remove_file(&full).map_err(|error| error.to_string())?;
        }
        let mut parent = full.parent();
        while let Some(dir) = parent {
            if dir == root {
                break;
            }
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
            parent = dir.parent();
        }
    }
    Ok(())
}

/// Restore the files touched by a turn back to before that turn:
/// 1. Try `git apply --reverse` on the full patch first — this preserves any edits made by the user or another
///    agent in unrelated files AND in non-overlapping regions of the same files.
/// 2. If a file has overlapping edits (or binary/add/delete divergence), reverse-apply every non-conflicting file
///    first and restore only the conflicting files directly from the `before` snapshot commit via a throwaway index.
///
/// Returns the paths that required direct snapshot fallback (empty when every file reversed cleanly).
pub fn restore(cwd: &Path, before: &str, patch: &str) -> Result<Vec<PathBuf>, String> {
    if patch.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root = root(cwd)?;
    let root = root.as_path();
    let Err(apply_err) = git_with(root, &["apply", "--reverse", "--whitespace=nowarn", "-"], Some(patch), &[]) else {
        return Ok(Vec::new());
    };
    let rev = format!("{before}^{{commit}}");
    if git_with(root, &["cat-file", "-e", &rev], None, &[]).is_err() {
        return Err(apply_err);
    }
    let file_patches = parse_file_patches(patch)?;
    if file_patches.is_empty() {
        return Err(apply_err);
    }
    let mut fallback_files = Vec::new();
    for file in file_patches {
        if git_with(root, &["apply", "--reverse", "--whitespace=nowarn", "-"], Some(&file.text), &[]).is_ok() {
            continue;
        }
        restore_file_from_snapshot(root, before, file.path())?;
        fallback_files.push(file.path().to_path_buf());
    }
    Ok(fallback_files)
}

/// Put one hunk back as it was in the snapshot. Fails, leaving the file alone, when the file has changed since the
/// hunk was read.
pub fn revert(cwd: &Path, hunk: &Hunk) -> Result<(), String> {
    git_with(&root(cwd)?, &["apply", "--reverse", "--whitespace=nowarn", "-"], Some(&hunk.patch), &[]).map(|_| ())
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
    fn checkpoint_restore_preserves_unrelated_and_non_overlapping_edits_and_handles_overlapping_edits() {
        let dir = repo();
        let touched = dir.path().join("note.txt");
        let unrelated = dir.path().join("other.txt");
        let binary = dir.path().join("new.bin");
        let lines: String = (1..=15).map(|n| format!("line {n}\n")).collect();
        std::fs::write(&touched, &lines).unwrap();
        std::fs::write(&unrelated, "orig\n").unwrap();
        commit_all(dir.path());

        let before = take(dir.path(), "s").unwrap();
        // Agent edits line 2 of note.txt and creates new.bin.
        std::fs::write(&touched, lines.replace("line 2\n", "agent 2\n")).unwrap();
        std::fs::write(&binary, [0, 1, 2, 3]).unwrap();
        let patch = capture(dir.path(), &before).unwrap();
        assert_eq!(parse(&patch).iter().map(|file| file.path.clone()).collect::<Vec<_>>(),
            [PathBuf::from("new.bin"), PathBuf::from("note.txt")]);

        // User/another model edits an unrelated file AND a non-overlapping line (line 14) of note.txt.
        std::fs::write(&unrelated, "user edit\n").unwrap();
        let with_user_edit = std::fs::read_to_string(&touched).unwrap().replace("line 14\n", "user 14\n");
        std::fs::write(&touched, &with_user_edit).unwrap();

        let fallback = restore(dir.path(), &before, &patch).unwrap();
        assert!(fallback.is_empty(), "non-overlapping edits should reverse-apply cleanly without fallback");
        assert!(!binary.exists(), "new binary file from the turn is removed");
        assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "user edit\n", "unrelated edit is untouched");
        let restored = std::fs::read_to_string(&touched).unwrap();
        assert!(restored.contains("line 2\n") && restored.contains("user 14\n"), "agent hunk reverted while user edit stayed: {restored}");

        // Now test overlapping edit on the exact same line: falls back to snapshot commit without touching index.
        let before2 = take(dir.path(), "s").unwrap();
        std::fs::write(&touched, "agent rewrite\n").unwrap();
        let patch2 = capture(dir.path(), &before2).unwrap();
        std::fs::write(&touched, "overlapping user edit\n").unwrap();
        let status_before = String::from_utf8(
            std::process::Command::new("git").args(["diff", "--cached"]).current_dir(dir.path()).output().unwrap().stdout,
        ).unwrap();
        let fallback2 = restore(dir.path(), &before2, &patch2).unwrap();
        assert_eq!(fallback2, vec![PathBuf::from("note.txt")]);
        assert_eq!(std::fs::read_to_string(&touched).unwrap(), restored);
        let status_after = String::from_utf8(
            std::process::Command::new("git").args(["diff", "--cached"]).current_dir(dir.path()).output().unwrap().stdout,
        ).unwrap();
        assert_eq!(status_after, status_before, "user git index must remain untouched");
    }

    #[test]
    fn checkpoint_restore_errors_when_patch_conflicts_and_snapshot_is_missing() {
        let dir = repo();
        let text = dir.path().join("note.txt");
        std::fs::write(&text, "before\n").unwrap();
        commit_all(dir.path());
        let before = take(dir.path(), "s").unwrap();
        std::fs::write(&text, "after\n").unwrap();
        let patch = capture(dir.path(), &before).unwrap();
        std::fs::write(&text, "conflict\n").unwrap();
        assert!(restore(dir.path(), "0000000000000000000000000000000000000000", &patch).is_err());
        assert_eq!(std::fs::read_to_string(&text).unwrap(), "conflict\n");
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

    #[test]
    fn file_patches_keep_each_files_own_paths_including_quoted_and_deleted_ones() {
        let patch = "diff --git a/one.txt b/one.txt\n--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-a\n+b\n\
diff --git \"a/say \\\"hi\\\".txt\" \"b/say \\\"hi\\\".txt\"\n--- \"a/say \\\"hi\\\".txt\"\n+++ \"b/say \\\"hi\\\".txt\"\n@@ -1 +1 @@\n-a\n+b\n\
diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n\
diff --git a/bin.dat b/bin.dat\nBinary files a/bin.dat and b/bin.dat differ\n";
        let files = parse_file_patches(patch).unwrap();
        let paths: Vec<_> = files.iter().map(|f| (f.old.clone(), f.new.clone())).collect();
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(paths, [
            (p("one.txt"), p("one.txt")),
            (p("say \"hi\".txt"), p("say \"hi\".txt")),
            (p("gone.txt"), None),
            (p("bin.dat"), p("bin.dat")),
        ]);
        assert_eq!(files[2].path(), Path::new("gone.txt"));
        assert!(files[1].text.starts_with("diff --git \"a/say") && files[1].text.ends_with("+b\n"));
    }

    #[test]
    fn a_file_patch_without_a_readable_path_fails_before_anything_changes() {
        assert!(parse_file_patches("diff --git weird\n--- what\n+++ ever\n@@ -1 +1 @@\n-a\n+b\n").is_err());
    }

    #[test]
    fn the_journal_puts_files_back_and_removes_ones_that_did_not_exist() {
        let dir = repo();
        std::fs::write(dir.path().join("kept.txt"), "before\n").unwrap();
        let patch = "diff --git a/kept.txt b/kept.txt\n--- a/kept.txt\n+++ b/kept.txt\n@@ -1 +1 @@\n-x\n+before\n\
diff --git a/sub/new.txt b/sub/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/sub/new.txt\n@@ -0,0 +1 @@\n+n\n";
        let journal = Journal::capture(dir.path(), patch).unwrap();
        std::fs::write(dir.path().join("kept.txt"), "changed\n").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/new.txt"), "n\n").unwrap();
        journal.put_back();
        assert_eq!(std::fs::read_to_string(dir.path().join("kept.txt")).unwrap(), "before\n");
        assert!(!dir.path().join("sub/new.txt").exists());
    }
}
