//! The files the editor shows: a folder listed without what Git ignores and with Git's changes marked, and a file read
//! as text when the editor can hold it. No GPUI here; callers run both off the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{Repository, Status, StatusOptions};

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
    /// The folder as listed (long, canonical form), which every `Node::path` starts with.
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    /// By `path_key`: files Git sees as changed or new, and the folders above them.
    pub changed: HashMap<String, Change>,
    /// The walk stopped at its limit, so some files are missing.
    pub truncated: bool,
}

/// List `root`, at most `limit` entries. Skips `.git` and whatever the repository ignores.
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
        _ => HashMap::new(),
    };
    Listing { root, nodes, changed, truncated: budget == 0 }
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
        nodes.push(Node { path, name, children: Some(children) });
    }
    nodes
}

/// What Git sees in a file since the last commit, staged or not. A folder takes the strongest change inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// New, whether untracked or staged.
    Added,
    /// Edited, renamed or deleted.
    Modified,
}

/// Changed and new files under `root`, and every folder between them and `root`.
fn changed(repo: &Repository, workdir: &Path, root: &Path) -> HashMap<String, Change> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true).include_ignored(false);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else { return HashMap::new() };
    let under_root = format!("{}/", path_key(root));
    let mut marks = HashMap::new();
    for entry in statuses.iter().filter(|s| !s.status().is_ignored()) {
        let Ok(path) = entry.path().map(|p| workdir.join(p)) else { continue };
        let change = if entry.status().intersects(Status::WT_NEW | Status::INDEX_NEW) { Change::Added } else { Change::Modified };
        for at in path.ancestors() {
            let key = path_key(at);
            if !key.starts_with(&under_root) {
                break;
            }
            // Its folders already carry at least this much.
            if marks.get(&key).is_some_and(|have| *have >= change) {
                break;
            }
            marks.insert(key, change);
        }
    }
    marks
}

/// What opening a file gives the editor.
#[derive(Debug, PartialEq)]
pub enum Contents {
    /// The editor gets the text as `format` describes, and `save` turns it back.
    Text { text: String, format: Format, stamp: Stamp },
    /// Over `MAX_BYTES` or `MAX_LINES`: the editor would stall on it.
    TooLarge,
    /// Not UTF-8 text (binary, or another encoding).
    NotText,
}

/// The editor widget handles text "up to about 50K lines" (gpui-base's own note).
pub const MAX_LINES: usize = 50_000;
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// A file's text exactly as stored, when the editor would open it.
fn read_stored(path: &Path, meta: &std::fs::Metadata) -> std::io::Result<Result<String, Contents>> {
    if meta.len() > MAX_BYTES {
        return Ok(Err(Contents::TooLarge));
    }
    let bytes = std::fs::read(path)?;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return Ok(Err(Contents::NotText));
    }
    let Ok(text) = String::from_utf8(bytes) else { return Ok(Err(Contents::NotText)) };
    if text.lines().count() > MAX_LINES {
        return Ok(Err(Contents::TooLarge));
    }
    Ok(Ok(text))
}

pub fn read(path: &Path) -> std::io::Result<Contents> {
    let meta = std::fs::metadata(path)?;
    // Taken before reading: a write that lands in between shows up as a change on the next check.
    let stamp = Stamp::of(&meta);
    let text = match read_stored(path, &meta)? {
        Ok(text) => text,
        Err(refused) => return Ok(refused),
    };
    let lf = text.matches('\n').count();
    // Mixed line ends are left as they are, so saving changes only what was edited.
    let crlf = lf > 0 && text.matches("\r\n").count() == lf;
    let text = if crlf { text.replace("\r\n", "\n") } else { text };
    let (text, tabs) = match untab(&text) {
        Some((shown, indents)) => (shown, Some(std::sync::Arc::new(indents))),
        None => (text, None),
    };
    Ok(Contents::Text { text, format: Format { crlf, tabs }, stamp })
}

/// How a file's text differs from what the editor shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Format {
    /// Every line ended in `\r\n`; the editor has `\n`.
    pub crlf: bool,
    /// The file is indented with tabs; the editor has spaces for each, because it draws a tab with no width.
    /// `None` for a file shown as it is.
    pub tabs: Option<std::sync::Arc<Indents>>,
}

/// How the editor shows a file with tabs, and what saving needs to give it back (see `untab`).
#[derive(Debug, Default, PartialEq)]
pub struct Indents {
    /// Most indented lines use tabs: new indents save as tabs too. Otherwise they stay spaces.
    pub hard: bool,
    /// The lines that saving gives back as they were, by how the editor shows them.
    kept: HashMap<String, Kept>,
}

#[derive(Debug, PartialEq)]
enum Kept {
    /// Every line that looks like this was this one.
    One(String),
    /// Different lines look the same (`\ta` and four spaces and `a`): each occurrence takes the next one, in file order.
    InOrder(Vec<String>),
}

/// Spaces per tab while a file is in the editor.
pub const TAB_WIDTH: usize = 4;

/// A file with tab-indented lines, as the editor shows it, because the editor draws a tab with no width: each tab as
/// spaces (those in a line's indent `TAB_WIDTH` each, the others up to the next multiple of `TAB_WIDTH`). Lines that
/// `ruled` would not give back as they were (tabs after the indent, spaces after the tabs to align, a template string
/// indented with spaces) are remembered, so they save unchanged until edited; an edited line keeps only the tabs in its
/// indent, and only in a file indented mostly with tabs. `None`, and the file is shown as it is, when no line is indented
/// with a tab (so a tab-separated table is never rewritten).
fn untab(text: &str) -> Option<(String, Indents)> {
    fn indent(l: &str) -> &str {
        &l[..l.len() - l.trim_start_matches([' ', '\t']).len()]
    }
    let tabbed = text.lines().filter(|l| indent(l).contains('\t')).count();
    if tabbed == 0 {
        return None;
    }
    let space_indented = text.lines().filter(|l| indent(l).len() >= TAB_WIDTH && !indent(l).contains('\t')).count();
    let hard = tabbed >= space_indented;
    let mut shown = String::with_capacity(text.len());
    let mut originals: HashMap<String, Vec<&str>> = HashMap::new();
    for line in text.split_inclusive('\n') {
        let (body, end) = line.strip_suffix('\n').map_or((line, ""), |b| (b, "\n"));
        let spaced = spaced(body);
        shown.push_str(&spaced);
        shown.push_str(end);
        originals.entry(spaced).or_default().push(body);
    }
    let mut kept = HashMap::new();
    for (spaced, bodies) in originals {
        if bodies.iter().all(|b| *b == bodies[0]) {
            if ruled(&spaced, hard) != bodies[0] {
                kept.insert(spaced, Kept::One(bodies[0].to_string()));
            }
        } else {
            kept.insert(spaced, Kept::InOrder(bodies.into_iter().map(String::from).collect()));
        }
    }
    Some((shown, Indents { hard, kept }))
}

/// The editor's text back as the file: lines `untab` remembered as they were, every other one by `ruled`.
fn retab(text: &str, indents: &Indents) -> String {
    let mut out = String::with_capacity(text.len());
    let mut taken: HashMap<&str, usize> = HashMap::new();
    for line in text.split_inclusive('\n') {
        let (body, end) = line.strip_suffix('\n').map_or((line, ""), |b| (b, "\n"));
        match indents.kept.get(body) {
            Some(Kept::One(original)) => out.push_str(original),
            Some(Kept::InOrder(originals)) => {
                let n = taken.entry(body).or_default();
                out.push_str(originals.get(*n).map_or_else(|| ruled(body, indents.hard), |o| o.clone()).as_str());
                *n += 1;
            }
            None => out.push_str(&ruled(body, indents.hard)),
        }
        out.push_str(end);
    }
    out
}

/// One line with each tab shown as spaces.
fn spaced(line: &str) -> String {
    let rest = line.trim_start_matches([' ', '\t']);
    let indent = &line[..line.len() - rest.len()];
    let mut out: String = indent.chars().map(|c| if c == '\t' { " ".repeat(TAB_WIDTH) } else { " ".into() }).collect();
    let mut column = out.chars().count();
    for c in rest.chars() {
        if c == '\t' {
            let width = TAB_WIDTH - column % TAB_WIDTH;
            out.push_str(&" ".repeat(width));
            column += width;
        } else {
            out.push(c);
            column += 1;
        }
    }
    out
}

/// One line with `TAB_WIDTH` leading spaces to a tab when the file is `hard` (indented with tabs), the rest staying spaces.
fn ruled(line: &str, hard: bool) -> String {
    if !hard {
        return line.to_string();
    }
    let rest = line.trim_start_matches(' ');
    let spaces = line.len() - rest.len();
    "\t".repeat(spaces / TAB_WIDTH) + &" ".repeat(spaces % TAB_WIDTH) + rest
}

/// When a file was last written and how long it was, to notice a change made by someone else (usually Claude).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self { modified: meta.modified().ok(), len: meta.len() }
    }
}

/// `None` when the file is gone (or can't be looked at).
pub fn stamp(path: &Path) -> Option<Stamp> {
    std::fs::metadata(path).ok().map(|m| Stamp::of(&m))
}

#[derive(Debug)]
pub enum SaveError {
    /// The file changed on disk since `expected`; nothing was written.
    Changed,
    Io(std::io::Error),
}

/// Write `text` over `path` in place (keeping its permissions and links), turned back into the file's `format`. With
/// `expected`, refuse when the file on disk no longer matches it. Returns the file's new stamp.
pub fn save(path: &Path, text: &str, format: Format, expected: Option<Stamp>) -> Result<Stamp, SaveError> {
    if expected.is_some() && stamp(path) != expected {
        return Err(SaveError::Changed);
    }
    let text = match &format.tabs {
        Some(indents) => std::borrow::Cow::Owned(retab(text, indents)),
        None => std::borrow::Cow::Borrowed(text),
    };
    let text = if format.crlf { std::borrow::Cow::Owned(text.replace('\n', "\r\n")) } else { text };
    std::fs::write(path, text.as_bytes()).map_err(SaveError::Io)?;
    stamp(path).ok_or_else(|| SaveError::Io(std::io::ErrorKind::NotFound.into()))
}

/// The highlighter's name for a file's language, from its extension; `None` shows it as plain text.
pub fn language(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" | "mdx" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "sh" | "bash" | "zsh" => "bash",
        "gleam" => "gleam",
        _ => return None,
    })
}

/// Why `name` can't name a new file or a renamed one; `Ok` when it can. One entry, no path: a name that Windows,
/// macOS or Linux would refuse or change is refused everywhere, so a project stays usable on all three.
pub fn check_name(name: &str) -> Result<(), String> {
    const RESERVED: [&str; 4] = ["con", "prn", "aux", "nul"];
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    let device = RESERVED.contains(&stem.as_str())
        || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.ends_with(|c: char| c.is_ascii_digit()));
    if name.trim().is_empty() {
        Err("Enter a name.".into())
    } else if name == "." || name == ".." {
        Err("That name is taken by the system.".into())
    } else if let Some(c) = name.chars().find(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        Err(format!("A name can't contain {}.", if c.is_control() { "control characters".to_string() } else { format!("“{c}”") }))
    } else if name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        Err("A name can't start with a space or end with a space or a dot.".into())
    } else if device {
        Err(format!("“{name}” is a device name on Windows."))
    } else {
        Ok(())
    }
}

/// `path` in its canonical form, with its last part left as it is (so a link is the link, not what it points to), when it
/// lies inside `root` (the folder itself only with `allow_root`); an error otherwise.
fn inside(root: &Path, path: &Path, allow_root: bool) -> std::io::Result<PathBuf> {
    let root = dunce::canonicalize(root)?;
    let path = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => dunce::canonicalize(parent)?.join(name),
        _ => dunce::canonicalize(path)?,
    };
    let is_root = path_key(&path) == path_key(&root);
    if (is_root && allow_root) || (!is_root && crate::platform::is_same_or_under(&path, &root)) {
        Ok(path)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "That is outside the session's folder."))
    }
}

fn bad_name(why: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, why)
}

/// Create the empty file `name` in folder `dir` (inside `root`); never over an existing one.
pub fn create_file(root: &Path, dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    check_name(name).map_err(bad_name)?;
    let path = inside(root, dir, true)?.join(name);
    std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    Ok(path)
}

/// Create the empty folder `name` in folder `dir` (inside `root`); never over an existing file or folder.
pub fn create_folder(root: &Path, dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    check_name(name).map_err(bad_name)?;
    let path = inside(root, dir, true)?.join(name);
    std::fs::create_dir(&path)?;
    Ok(path)
}

/// Rename `path` (inside `root`) to `name` in the same folder; never over another file. A change of case alone is
/// allowed where the file system ignores case.
pub fn rename(root: &Path, path: &Path, name: &str) -> std::io::Result<PathBuf> {
    check_name(name).map_err(bad_name)?;
    let from = inside(root, path, false)?;
    let to = from.with_file_name(name);
    // `fs::rename` replaces an existing file on macOS and Linux, so look first.
    if to.symlink_metadata().is_ok() && !same_file(&to, &from) {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("“{name}” already exists.")));
    }
    std::fs::rename(&from, &to)?;
    Ok(to)
}

/// Whether two paths name the same file, as a change of case does where the file system ignores case. By inode on
/// macOS and Linux, where `path_key` keeps case although macOS usually ignores it.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (a.symlink_metadata(), b.symlink_metadata()) {
            (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        path_key(a) == path_key(b)
    }
}

/// Delete the file or folder `path` (inside `root`, never `root` itself), a folder with everything in it. A link is
/// removed, not what it points to.
pub fn delete(root: &Path, path: &Path) -> std::io::Result<()> {
    let path = inside(root, path, false)?;
    if path.symlink_metadata()?.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }
}

/// Every file in a listing, folders left out.
pub fn files_of(nodes: &[Node]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for n in nodes {
        match &n.children {
            Some(children) => out.extend(files_of(children)),
            None => out.push(n.path.clone()),
        }
    }
    out
}

/// Quick open: the indexes of `paths` (relative, `/`-separated) that hold the letters of `query` in order, best first.
/// Matches in the file name beat matches in folders, then letters that follow each other, then shorter paths.
pub fn rank_paths(query: &str, paths: &[String], limit: usize) -> Vec<usize> {
    let query: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let mut scored: Vec<(i64, usize)> = paths.iter().enumerate().filter_map(|(i, p)| fuzzy_score(&query, p).map(|s| (s, i))).collect();
    scored.sort_by_key(|(score, i)| (std::cmp::Reverse(*score), paths[*i].len(), *i));
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

fn fuzzy_score(query: &[char], path: &str) -> Option<i64> {
    let lower: Vec<char> = path.to_lowercase().chars().collect();
    let name_start = lower.iter().rposition(|c| *c == '/').map_or(0, |i| i + 1);
    let (mut score, mut at, mut prev) = (0i64, 0usize, None::<usize>);
    for q in query {
        let found = at + lower[at..].iter().position(|c| c == q)?;
        score += 1;
        if found >= name_start {
            score += 4;
        }
        if prev.is_some_and(|p| p + 1 == found) {
            score += 3;
        }
        prev = Some(found);
        at = found + 1;
    }
    Some(score)
}

/// A line that holds the searched text.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    /// 0-based.
    pub line: usize,
    pub text: String,
}

/// Find in files: lines of `files` that contain `query`, ignoring case, at most `limit`. Files `read` refuses are skipped.
pub fn search(files: &[PathBuf], query: &str, limit: usize, stale: &dyn Fn() -> bool) -> Vec<Hit> {
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    for path in files {
        // A newer query replaced this one: its hits would be thrown away.
        if stale() {
            return hits;
        }
        // Line numbers and trimmed lines come out the same as in the editor, which only changes line ends and indents.
        let Ok(Ok(text)) = std::fs::metadata(path).and_then(|meta| read_stored(path, &meta)) else { continue };
        for (line, l) in text.lines().enumerate().filter(|(_, l)| l.to_lowercase().contains(&needle)) {
            if hits.len() == limit {
                return hits;
            }
            hits.push(Hit { path: path.clone(), line, text: l.trim().chars().take(200).collect() });
        }
    }
    hits
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
        assert_eq!(names(&listing.nodes), [String::from("empty"), "src".into(), ".gitignore".into(), "README.md".into()]);
        assert_eq!(find(&listing.nodes, "empty").children, Some(vec![]), "an empty folder is still a folder");
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

        fs::write(root.join("docs/c.md"), "new").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("docs/c.md")).unwrap();
        index.write().unwrap();
        fs::create_dir_all(root.join("fresh")).unwrap();
        fs::write(root.join("fresh/d.md"), "new").unwrap();

        let changed = list(&root, 1000).changed;
        let mark = |p: &str| changed.get(&path_key(&root.join(p))).copied();
        for p in ["src/deep/a.rs", "src/deep", "src"] {
            assert_eq!(mark(p), Some(Change::Modified), "{p}");
        }
        for p in ["new.txt", "docs/c.md", "docs", "fresh/d.md", "fresh"] {
            assert_eq!(mark(p), Some(Change::Added), "{p}: untracked and staged files are both new");
        }
        assert_eq!(mark("docs/b.md"), None);
        assert_eq!(mark(""), None, "the listed folder itself");
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
        assert!(matches!(read(&p("a.rs")).unwrap(), Contents::Text { text, format: Format { crlf: false, tabs: None }, .. } if text == "fn main() {}\n"));
        assert_eq!(read(&p("bin")).unwrap(), Contents::NotText);
        assert_eq!(read(&p("latin1.txt")).unwrap(), Contents::NotText);
        assert_eq!(read(&p("long.txt")).unwrap(), Contents::TooLarge);
        assert!(read(&p("missing")).is_err());
    }
    #[test]
    fn quick_open_prefers_the_file_name_and_letters_in_a_row() {
        let paths: Vec<String> = ["src/app/mod.rs", "src/main.rs", "README.md", "src/panels/sidebar.rs", "web/src/pages/index.astro", "modules/a.rs"].map(String::from).into();
        let ranked = |q: &str| rank_paths(q, &paths, 10).into_iter().map(|i| paths[i].as_str()).collect::<Vec<_>>();
        assert_eq!(ranked("main")[0], "src/main.rs");
        assert_eq!(ranked("side")[0], "src/panels/sidebar.rs");
        assert_eq!(ranked("mod")[0], "src/app/mod.rs", "a match in the file name beats a shorter path");
        assert_eq!(ranked("SRCMOD"), ["src/app/mod.rs"], "case is ignored and letters may be apart");
        assert!(ranked("zzz").is_empty());
        assert_eq!(ranked("").len(), paths.len(), "an empty query lists everything");
        assert_eq!(rank_paths("s", &paths, 2).len(), 2);
    }

    #[test]
    fn find_in_files_reports_the_lines_the_editor_shows() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("tabs.rs");
        fs::write(&file, "fn a() {
	let total = 1;
}
").unwrap();
        let hits = search(&[file.clone()], "total", 100, &|| false);
        assert_eq!(hits, [Hit { path: file.clone(), line: 1, text: "let total = 1;".into() }]);
        let Ok(Contents::Text { text, .. }) = read(&file) else { panic!("readable") };
        assert_eq!(text.lines().nth(hits[0].line).unwrap().trim(), hits[0].text, "the editor has the same line there");
    }

    #[test]
    fn find_in_files_lists_matching_lines_and_skips_what_it_cannot_read() {
        let tmp = tempfile::tempdir().unwrap();
        let p = |n: &str| tmp.path().join(n);
        fs::write(p("a.rs"), "fn main() {
    let Total = 1;
}
").unwrap();
        fs::write(p("b.md"), "the total
nothing
").unwrap();
        fs::write(p("bin"), [b't', b'o', b't', b'a', b'l', 0]).unwrap();
        let files = vec![p("a.rs"), p("b.md"), p("bin")];
        let hits = search(&files, "total", 100, &|| false);
        assert_eq!(hits, [
            Hit { path: p("a.rs"), line: 1, text: "let Total = 1;".into() },
            Hit { path: p("b.md"), line: 0, text: "the total".into() },
        ]);
        assert_eq!(search(&files, "total", 1, &|| false).len(), 1);
        assert!(search(&files, "", 100, &|| false).is_empty());
        assert!(search(&files, "total", 100, &|| true).is_empty(), "a stale search reads nothing");
    }

    #[test]
    fn languages_come_from_the_extension() {
        for (file, lang) in [("src/main.rs", Some("rust")), ("a.TS", Some("typescript")), ("app.tsx", Some("tsx")), ("x.mjs", Some("javascript")), ("Cargo.toml", Some("toml")), ("ci.yml", Some("yaml")), ("README.md", Some("markdown")), ("app.gleam", Some("gleam")), ("run.sh", Some("bash")), ("notes.txt", None), ("Makefile", None)] {
            assert_eq!(language(Path::new(file)), lang, "{file}");
        }
    }

    #[test]
    fn files_of_a_listing_leaves_folders_out() {
        let f = |n: &str| Node { path: n.into(), name: n.into(), children: None };
        let nodes = vec![Node { path: "src".into(), name: "src".into(), children: Some(vec![f("src/a.rs")]) }, f("b.md")];
        assert_eq!(files_of(&nodes), [PathBuf::from("src/a.rs"), PathBuf::from("b.md")]);
    }

    #[test]
    fn crlf_files_are_edited_with_lf_and_saved_back_with_crlf() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("win.txt");
        fs::write(&p, "a\r\nb\r\n").unwrap();
        let Contents::Text { text, format, stamp: read_stamp } = read(&p).unwrap() else { panic!("not text") };
        assert_eq!((text.as_str(), format.crlf), ("a\nb\n", true));
        save(&p, "a\nb\nc\n", format, Some(read_stamp)).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\r\nb\r\nc\r\n");

        fs::write(&p, "a\r\nb\n").unwrap();
        let Contents::Text { text, format, .. } = read(&p).unwrap() else { panic!("not text") };
        assert_eq!((text.as_str(), format.crlf), ("a\r\nb\n", false), "mixed line ends are left alone");
    }

    #[test]
    fn tab_indents_are_edited_as_spaces_and_saved_back_as_tabs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.ts");
        let file = "class A {\r\n\t/**\r\n\t * Doc.\r\n\t */\r\n\tf() {\r\n\t\treturn\t1;\r\n\t}\r\n}\r\n";
        fs::write(&p, file).unwrap();
        let Contents::Text { text, format, stamp: s } = read(&p).unwrap() else { panic!("not text") };
        assert!(format.crlf && format.tabs.is_some());
        assert_eq!(text, "class A {\n    /**\n     * Doc.\n     */\n    f() {\n        return  1;\n    }\n}\n", "every tab becomes spaces");
        let s = save(&p, &text, format.clone(), Some(s)).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), file, "an unedited file saves unchanged");
        save(&p, "a\n      b\n", format, Some(s)).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\r\n\t  b\r\n", "new indents become tabs too");

        // Tab-indented files with lines whose indent doesn't turn back by the rule: shown without tabs, saved unchanged.
        let jsx = "export function App() {\n\treturn (\n\t\t<div\n\t\t\tonClick={() =>\n\t\t\t    go()}\n\t\t/>\n\t);\n}\nconst css = `\n    color: red;\n`;\n";
        for (file, why) in [(jsx, "aligned with spaces after the tabs, and a template string indented with spaces"), ("\ta\n    b\n", "a line indented with spaces"), ("\t    a\n", "spaces after the tabs"), (" \ta\n", "a tab after spaces")] {
            fs::write(&p, file).unwrap();
            let Contents::Text { text, format, stamp: s } = read(&p).unwrap() else { panic!("not text") };
            assert!(format.tabs.is_some() && !text.contains('\t'), "{why}: shown without tabs: {text:?}");
            save(&p, &text, format, Some(s)).unwrap();
            assert_eq!(fs::read_to_string(&p).unwrap(), file, "{why}: saved unchanged");
        }
        // An edited line takes the rule; the lines around it keep their own indents.
        fs::write(&p, jsx).unwrap();
        let Contents::Text { text, format, stamp: s } = read(&p).unwrap() else { panic!("not text") };
        save(&p, &text.replace("return (", "return  ("), format, Some(s)).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), jsx.replace("return (", "return  ("));

        for (file, why) in [("a\n    b\n", "no tabs"), ("a\tb\n    c\n", "tabs only inside lines")] {
            fs::write(&p, file).unwrap();
            let Contents::Text { text, format, .. } = read(&p).unwrap() else { panic!("not text") };
            assert_eq!((text.as_str(), format.tabs.is_none()), (file, true), "{why}: left alone");
        }
    }

    #[test]
    fn tabs_after_the_indent_are_shown_too() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.go");
        for (file, shown, why) in [
            ("type A struct {\n\tName\tstring\n\tId\tint\n}\n", "type A struct {\n    Name    string\n    Id  int\n}\n", "tabs that align"),
        ] {
            fs::write(&p, file).unwrap();
            let Contents::Text { text, format, stamp: s } = read(&p).unwrap() else { panic!("not text") };
            assert_eq!(text, shown, "{why}");
            save(&p, &text, format, Some(s)).unwrap();
            assert_eq!(fs::read_to_string(&p).unwrap(), file, "{why}: saved unchanged");
        }
        // A line edited by hand keeps the tabs of its indent, not the ones inside it.
        fs::write(&p, "type A struct {\n\tName\tstring\n}\n").unwrap();
        let Contents::Text { format, stamp: s, .. } = read(&p).unwrap() else { panic!("not text") };
        save(&p, "type A struct {\n    Name    int\n}\n", format, Some(s)).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "type A struct {\n\tName    int\n}\n");
        // Lines that look the same but differ, and a file mostly indented with spaces, still show their tabs and save as they were.
        for file in ["\t\ta\n\t    a\n    \t\ta\n", "a\n    b\n    c\n\td\n    e\n"] {
            fs::write(&p, file).unwrap();
            let Contents::Text { text, format, stamp: s } = read(&p).unwrap() else { panic!("not text") };
            assert!(!text.contains('\t'), "{text:?}");
            save(&p, &text, format, Some(s)).unwrap();
            assert_eq!(fs::read_to_string(&p).unwrap(), file);
        }
        // Without tab indents, tabs are the file's data (a table): it is shown and saved as it is.
        fs::write(&p, "a\tb\nc\td\n").unwrap();
        let Contents::Text { text, format, .. } = read(&p).unwrap() else { panic!("not text") };
        assert_eq!((text.as_str(), format.tabs.is_none()), ("a\tb\nc\td\n", true));
    }

    #[test]
    fn saving_refuses_to_overwrite_a_file_changed_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.rs");
        fs::write(&p, "one\n").unwrap();
        let Contents::Text { stamp: read_stamp, .. } = read(&p).unwrap() else { panic!("not text") };
        fs::write(&p, "changed by Claude\n").unwrap();
        assert!(matches!(save(&p, "mine\n", Format::default(), Some(read_stamp)), Err(SaveError::Changed)));
        assert_eq!(fs::read_to_string(&p).unwrap(), "changed by Claude\n", "nothing was written");

        let newer = save(&p, "mine\n", Format::default(), None).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "mine\n", "without an expected stamp it overwrites");
        assert_eq!(stamp(&p), Some(newer));
        fs::remove_file(&p).unwrap();
        assert_eq!(stamp(&p), None);
    }

    #[test]
    fn a_name_is_one_plain_entry() {
        for good in ["main.rs", ".env", "a b.txt", "notes"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        for bad in ["", "  ", ".", "..", "a/b", "a\\b", "x:y", "what?", "nul\0", "trailing.", "CON", "nul.txt", "com1.rs"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn creates_a_file_and_never_overwrites_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let root = root.as_path();
        fs::create_dir(root.join("src")).unwrap();
        let made = create_file(root, &root.join("src"), "new.rs").unwrap();
        assert_eq!(made, root.join("src/new.rs"));
        assert_eq!(fs::read_to_string(&made).unwrap(), "");

        fs::write(&made, "keep").unwrap();
        assert_eq!(create_file(root, &root.join("src"), "new.rs").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&made).unwrap(), "keep");
        assert!(create_file(root, &root.join("src"), "../escape.rs").is_err());
        let outside = tempfile::tempdir().unwrap();
        assert!(create_file(root, outside.path(), "x.rs").is_err(), "only inside the session folder");
    }

    #[test]
    fn creates_a_folder_and_never_over_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let root = root.as_path();
        let made = create_folder(root, root, "lib").unwrap();
        assert_eq!(made, root.join("lib"));
        assert!(made.is_dir());

        fs::write(made.join("keep.rs"), "keep").unwrap();
        assert_eq!(create_folder(root, root, "lib").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(made.join("keep.rs")).unwrap(), "keep");
        fs::write(root.join("file"), "").unwrap();
        assert_eq!(create_folder(root, root, "file").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert!(create_folder(root, root, "a/b").is_err());
        let outside = tempfile::tempdir().unwrap();
        assert!(create_folder(root, outside.path(), "x").is_err(), "only inside the session folder");
    }

    #[test]
    fn renames_in_place_and_never_over_another_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let root = root.as_path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "a").unwrap();
        fs::write(root.join("src/b.rs"), "b").unwrap();

        let moved = rename(root, &root.join("src/a.rs"), "c.rs").unwrap();
        assert_eq!(moved, root.join("src/c.rs"));
        assert_eq!(fs::read_to_string(&moved).unwrap(), "a");
        assert!(!root.join("src/a.rs").exists());

        assert_eq!(rename(root, &moved, "b.rs").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(root.join("src/b.rs")).unwrap(), "b", "the other file is untouched");
        assert!(rename(root, &moved, "../c.rs").is_err());

        let recased = rename(root, &moved, "C.rs").unwrap();
        assert_eq!(recased.file_name().unwrap(), "C.rs", "a change of case alone is a rename, not a clash");
        assert_eq!(fs::read_to_string(&recased).unwrap(), "a");
    }

    #[test]
    fn deletes_files_and_folders_inside_the_session_folder_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::write(root.join("src/deep/x.rs"), "x").unwrap();
        fs::write(root.join("top.rs"), "t").unwrap();

        delete(root, &root.join("top.rs")).unwrap();
        assert!(!root.join("top.rs").exists());
        delete(root, &root.join("src")).unwrap();
        assert!(!root.join("src").exists(), "a folder goes with everything in it");

        assert!(delete(root, root).is_err(), "never the session folder itself");
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("o.rs"), "o").unwrap();
        assert!(delete(root, &outside.path().join("o.rs")).is_err());
        assert!(outside.path().join("o.rs").exists());
    }
}
