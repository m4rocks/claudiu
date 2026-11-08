//! Which lines of a file differ from the last commit, for the editor's gutter. Staged and unstaged changes both
//! count, and a file Git doesn't know yet is all new.

use std::ops::Range;
use std::path::{Path, PathBuf};

use git2::{DiffOptions, Repository};

use crate::platform::{is_same_or_under, normalize};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Added,
    Modified,
    /// Lines were removed just above `Change::lines.start`.
    Deleted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// 0-based lines of the file as it is now; empty for `Deleted`, where `start` is the line below the removal.
    pub lines: Range<usize>,
    pub kind: Kind,
}

/// The changes in `file` since HEAD, in file order. Empty when the file isn't in a repository.
pub fn changed_lines(file: &Path) -> Vec<Change> {
    let Some(dir) = file.parent() else { return vec![] };
    let Ok(repo) = Repository::discover(dir) else { return vec![] };
    let Some(root) = repo.workdir().filter(|root| is_same_or_under(file, root)) else { return vec![] };
    // Cut the repository's folders off by count, not by text: libgit2 reports the folder's real casing, which on Windows
    // may differ from how the file was reached.
    let relative: PathBuf = normalize(file).components().skip(normalize(root).components().count()).collect();
    let mut options = DiffOptions::new();
    // The path is one file, not a pattern: `[id].tsx` or `a*b` must not also match their neighbours. Git paths use `/`.
    let relative = relative.to_string_lossy();
    let relative = if cfg!(windows) { relative.replace('\\', "/") } else { relative.into_owned() };
    options.pathspec(relative).disable_pathspec_match(true).context_lines(0).include_untracked(true).show_untracked_content(true);
    // A repository without commits has no HEAD tree: everything in it is new.
    let head = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let Ok(diff) = repo.diff_tree_to_workdir_with_index(head.as_ref(), Some(&mut options)) else { return vec![] };
    let mut changes = Vec::new();
    let _ = diff.foreach(
        &mut |_, _| true,
        None,
        Some(&mut |_, hunk| {
            let (old, new, start) = (hunk.old_lines() as usize, hunk.new_lines() as usize, hunk.new_start() as usize);
            changes.push(if new == 0 {
                // `new_start` counts lines from 1 and names the line above the removal, so as a 0-based index it is the line below.
                Change { lines: start..start, kind: Kind::Deleted }
            } else {
                Change { lines: start - 1..start - 1 + new, kind: if old == 0 { Kind::Added } else { Kind::Modified } }
            });
            true
        }),
        None,
    );
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_all(repo: &Repository) {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let parents: Vec<_> = repo.head().ok().and_then(|h| h.peel_to_commit().ok()).into_iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &parents.iter().collect::<Vec<_>>()).unwrap();
    }

    #[test]
    fn added_modified_and_deleted_lines_are_told_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\nf\n").unwrap();
        commit_all(&repo);
        assert_eq!(changed_lines(&file), vec![], "nothing changed yet");

        // a -> A, d removed, g appended
        std::fs::write(&file, "A\nb\nc\ne\nf\ng\n").unwrap();
        assert_eq!(
            changed_lines(&file),
            vec![
                Change { lines: 0..1, kind: Kind::Modified },
                Change { lines: 3..3, kind: Kind::Deleted },
                Change { lines: 5..6, kind: Kind::Added },
            ]
        );
    }

    #[test]
    fn a_file_in_a_folder_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let repo = Repository::init(&dir).unwrap();
        std::fs::create_dir_all(dir.join("src").join("deep")).unwrap();
        let file = dir.join("src").join("deep").join("f.txt");
        std::fs::write(&file, "a\nb\n").unwrap();
        commit_all(&repo);
        std::fs::write(&file, "a\nB\n").unwrap();
        assert_eq!(changed_lines(&file), vec![Change { lines: 1..2, kind: Kind::Modified }]);
    }

    #[test]
    fn a_name_with_glob_characters_is_not_a_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let repo = Repository::init(&dir).unwrap();
        std::fs::write(dir.join("[ab].txt"), "x\n").unwrap();
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        commit_all(&repo);
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        assert_eq!(changed_lines(&dir.join("[ab].txt")), vec![], "a.txt's change is not [ab].txt's");
    }

    #[test]
    fn a_file_git_does_not_know_yet_is_all_new() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let repo = Repository::init(&dir).unwrap();
        std::fs::write(dir.join("old.txt"), "x\n").unwrap();
        commit_all(&repo);
        let file = dir.join("new.txt");
        std::fs::write(&file, "1\n2\n3\n").unwrap();
        assert_eq!(changed_lines(&file), vec![Change { lines: 0..3, kind: Kind::Added }]);
    }

    #[test]
    fn a_file_outside_a_repository_has_no_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f.txt");
        std::fs::write(&file, "x\n").unwrap();
        assert_eq!(changed_lines(&file), vec![]);
    }

    #[cfg(windows)]
    #[test]
    fn a_file_reached_through_a_differently_cased_folder_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "a
b
").unwrap();
        commit_all(&repo);
        std::fs::write(&file, "a
B
").unwrap();
        let shouted = std::path::PathBuf::from(dir.to_string_lossy().to_uppercase()).join("f.txt");
        assert_eq!(changed_lines(&shouted), vec![Change { lines: 1..2, kind: Kind::Modified }]);
    }
}
