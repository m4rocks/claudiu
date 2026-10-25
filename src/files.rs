//! The files the editor shows: a folder listed without what Git ignores and with Git's changes marked, and a file read
//! as text when the editor can hold it. No GPUI here; callers run both off the UI thread.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use git2::{Repository, StatusOptions};

use crate::platform::path_key;

/// A file, or a folder with what it holds (folders first, then by name).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    /// `None` for a file.
    pub children: Option<Vec<Node>>,
}

pub struct Listing {
    pub nodes: Vec<Node>,
    /// `path_key`s of files Git sees as changed or new, and of the folders above them.
    pub changed: HashSet<String>,
    /// The walk stopped at its limit, so some files are missing.
    pub truncated: bool,
}

/// List `root`, at most `limit` entries. Skips `.git`, whatever the repository ignores and folders left empty.
pub fn list(root: &Path, limit: usize) -> Listing {
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let repo = Repository::discover(&root).ok();
    let workdir = repo.as_ref().and_then(|r| r.workdir()).map(Path::to_path_buf);
    let ignored = |path: &Path| match (&repo, &workdir) {
        (Some(repo), Some(wd)) => path.strip_prefix(wd).is_ok_and(|rel| repo.is_path_ignored(rel).unwrap_or(false)),
        _ => false,
    };
    let mut budget = limit;
    let nodes = walk(&root, &ignored, &mut budget);
    let changed = match (&repo, &workdir) {
        (Some(repo), Some(wd)) => changed(repo, wd, &root),
        _ => HashSet::new(),
    };
    Listing { nodes, changed, truncated: budget == 0 }
}

fn walk(dir: &Path, ignored: &dyn Fn(&Path) -> bool, budget: &mut usize) -> Vec<Node> {
    let Ok(read) = std::fs::read_dir(dir) else { return vec![] };
    let mut entries: Vec<(PathBuf, String, bool)> = read
        .flatten()
        .map(|e| (e.path(), e.file_name().to_string_lossy().into_owned(), e.file_type().is_ok_and(|t| t.is_dir())))
        .filter(|(path, name, _)| name != ".git" && !ignored(path))
        .collect();
    entries.sort_by_cached_key(|(_, name, is_dir)| (!is_dir, name.to_lowercase()));
    let mut nodes = Vec::new();
    for (path, name, is_dir) in entries {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        if !is_dir {
            nodes.push(Node { path, name, children: None });
            continue;
        }
        let children = walk(&path, ignored, budget);
        // The tree draws a folder without children as a file.
        if !children.is_empty() {
            nodes.push(Node { path, name, children: Some(children) });
        }
    }
    nodes
}

/// Changed and untracked files under `root`, and every folder between them and `root`.
fn changed(repo: &Repository, workdir: &Path, root: &Path) -> HashSet<String> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true).include_ignored(false);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else { return HashSet::new() };
    let root_key = path_key(root);
    let mut keys = HashSet::new();
    for path in statuses.iter().filter(|s| !s.status().is_ignored()).filter_map(|s| s.path().ok().map(|p| workdir.join(p))) {
        for at in path.ancestors() {
            let key = path_key(at);
            if key == root_key || !key.starts_with(&root_key) || !keys.insert(key) {
                break;
            }
        }
    }
    keys
}

/// What opening a file gives the editor.
#[derive(Debug, PartialEq)]
pub enum Contents {
    Text(String),
    /// Over `MAX_BYTES` or `MAX_LINES`: the editor would stall on it.
    TooLarge,
    /// Not UTF-8 text (binary, or another encoding).
    NotText,
}

/// The editor widget handles text "up to about 50K lines" (gpui-base's own note).
pub const MAX_LINES: usize = 50_000;
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

pub fn read(path: &Path) -> std::io::Result<Contents> {
    if std::fs::metadata(path)?.len() > MAX_BYTES {
        return Ok(Contents::TooLarge);
    }
    let bytes = std::fs::read(path)?;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return Ok(Contents::NotText);
    }
    let Ok(text) = String::from_utf8(bytes) else { return Ok(Contents::NotText) };
    if text.lines().count() > MAX_LINES {
        return Ok(Contents::TooLarge);
    }
    Ok(Contents::Text(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(nodes: &[Node]) -> Vec<String> {
        nodes.iter().map(|n| n.name.clone()).collect()
    }

    fn find<'a>(nodes: &'a [Node], name: &str) -> &'a Node {
        nodes.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("{name} missing from {:?}", names(nodes)))
    }

    fn commit_all(repo: &Repository) {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    }

    #[test]
    fn lists_folders_first_without_ignored_files_or_git() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("target/debug/app"), "bin").unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("debug.log"), "x").unwrap();
        fs::write(root.join("README.md"), "# hi").unwrap();
        Repository::init(root).unwrap();

        let listing = list(root, 1000);
        assert_eq!(names(&listing.nodes), [String::from("src"), ".gitignore".into(), "README.md".into()]);
        assert_eq!(names(find(&listing.nodes, "src").children.as_ref().unwrap()), ["main.rs"]);
        assert!(find(&listing.nodes, "README.md").children.is_none());
        assert!(!listing.truncated);
    }

    #[test]
    fn marks_changed_and_new_files_and_their_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("src/deep/a.rs"), "a").unwrap();
        fs::write(root.join("docs/b.md"), "b").unwrap();
        let repo = Repository::init(&root).unwrap();
        commit_all(&repo);
        fs::write(root.join("src/deep/a.rs"), "changed").unwrap();
        fs::write(root.join("new.txt"), "new").unwrap();

        let changed = list(&root, 1000).changed;
        for p in ["src/deep/a.rs", "src/deep", "src", "new.txt"] {
            assert!(changed.contains(&path_key(&root.join(p))), "{p} is not marked: {changed:?}");
        }
        for p in ["docs", "docs/b.md"] {
            assert!(!changed.contains(&path_key(&root.join(p))), "{p} is marked");
        }
    }

    #[test]
    fn a_plain_folder_is_listed_and_the_limit_stops_the_walk() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10 {
            fs::write(tmp.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        assert_eq!(list(tmp.path(), 1000).nodes.len(), 10);
        let capped = list(tmp.path(), 4);
        assert_eq!(capped.nodes.len(), 4);
        assert!(capped.truncated);
    }

    #[test]
    fn reads_text_and_refuses_what_the_editor_cannot_hold() {
        let tmp = tempfile::tempdir().unwrap();
        let p = |n: &str| tmp.path().join(n);
        fs::write(p("a.rs"), "fn main() {}\n").unwrap();
        fs::write(p("bin"), [0x7f, b'E', b'L', b'F', 0, 0, 1]).unwrap();
        fs::write(p("latin1.txt"), [b'c', b'a', b'f', 0xe9]).unwrap();
        fs::write(p("long.txt"), "x\n".repeat(MAX_LINES + 1)).unwrap();
        assert_eq!(read(&p("a.rs")).unwrap(), Contents::Text("fn main() {}\n".into()));
        assert_eq!(read(&p("bin")).unwrap(), Contents::NotText);
        assert_eq!(read(&p("latin1.txt")).unwrap(), Contents::NotText);
        assert_eq!(read(&p("long.txt")).unwrap(), Contents::TooLarge);
        assert!(read(&p("missing")).is_err());
    }
}
