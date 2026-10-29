//! Sessions in git worktrees. Claudiu makes a worktree with `git worktree add` in its own data folder, away from the
//! repository, so `git status` never lists it; a session moves into one by resuming there (see `app/switch.rs`).
//! This file holds the names, the folders, and making and removing them.

use std::path::{Path, PathBuf};

use git2::Repository;

use super::GitInfo;
use super::commit::{git, has_origin, origin_default_branch};
use crate::platform::path_key;

/// Where Claudiu keeps the worktrees it makes: one folder per repository, one folder per worktree inside it.
pub fn root() -> PathBuf {
    crate::datadir::data_dir().join("worktrees")
}

/// What a new worktree's branch starts from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Base {
    /// Origin's default branch, fetched first; the current commit when there is no origin.
    #[default]
    Main,
    /// The commit the main checkout is on now.
    Current,
}

/// The folder under `root` that holds a repository's worktrees: its name, and a hash of its path to tell apart two
/// repositories of the same name. FNV-1a is written out because the folder must come out the same on every run.
fn repo_folder(root: &Path, main_checkout: &Path) -> PathBuf {
    let hash = path_key(main_checkout).bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3));
    let name = main_checkout.file_name().map_or_else(|| "repo".to_string(), |n| crate::datadir::safe_file_stem(&n.to_string_lossy()));
    root.join(format!("{name}-{:08x}", hash as u32))
}

/// Where the worktree called `name` of this repository goes.
pub fn folder(root: &Path, main_checkout: &Path, name: &str) -> PathBuf {
    repo_folder(root, main_checkout).join(name)
}

/// A worktree folder under `root`, as the Worktrees page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Made {
    /// The repository's folder name under `root`, such as `app-1a2b3c4d`.
    pub repo: String,
    pub folder: PathBuf,
}

/// Every worktree folder under `root`, by repository and then name. Blocking.
pub fn made(root: &Path) -> Vec<Made> {
    let folders_in = |dir: &Path| {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        found.sort();
        found
    };
    folders_in(root)
        .into_iter()
        .flat_map(|repo| {
            let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            folders_in(&repo).into_iter().map(move |folder| Made { repo: name.clone(), folder })
        })
        .collect()
}

/// The repository's name in a folder from `repo_folder`, without the hash.
pub fn repo_name(repo_folder: &str) -> &str {
    repo_folder.rsplit_once('-').map_or(repo_folder, |(name, _)| name)
}

/// Whether `folder` is the worktree folder `repo_folder` holds for this repository.
pub fn belongs_to(root: &Path, main_checkout: &Path, folder: &Path) -> bool {
    folder.parent().is_some_and(|parent| path_key(parent) == path_key(&repo_folder(root, main_checkout)))
}

/// The name becomes a folder and a branch, so it is refused rather than rewritten when it holds anything else.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Give the worktree a name.".into());
    }
    // A leading dash would be read as an option, and git refuses it in a branch name too.
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !name.starts_with('-') {
        Ok(())
    } else {
        Err("Use letters, digits, - and _ in a worktree name, and don't start with -.".into())
    }
}

const ADJECTIVES: [&str; 16] = [
    "bright", "calm", "eager", "gentle", "happy", "keen", "lively", "mellow", "nimble", "proud", "quick", "quiet", "steady", "swift", "tidy", "warm",
];
const ANIMALS: [&str; 16] = [
    "badger", "crane", "falcon", "fox", "heron", "lynx", "marten", "otter", "owl", "panda", "robin", "seal", "stoat", "swan", "wolf", "wren",
];

/// A two-word name such as `bright-fox`, picked by `seed` and moved on until `taken` is false.
pub fn generate_name(seed: u128, taken: impl Fn(&str) -> bool) -> String {
    let combinations = (ADJECTIVES.len() * ANIMALS.len()) as u128;
    let start = seed % combinations;
    (0..combinations)
        .map(|step| {
            let pick = ((start + step) % combinations) as usize;
            format!("{}-{}", ADJECTIVES[pick / ANIMALS.len()], ANIMALS[pick % ANIMALS.len()])
        })
        .find(|name| !taken(name))
        // Every one of the 256 names is in use: a numbered one is not among them.
        .unwrap_or_else(|| format!("worktree-{start}"))
}

/// The main checkout a new worktree would branch from, or why there is none.
pub fn source(git: Option<&GitInfo>) -> Result<&Path, &'static str> {
    let git = git.ok_or("is not a git repository")?;
    if git.unborn {
        return Err("has no commits yet");
    }
    Ok(git.main_checkout())
}

/// What the top-bar pill calls the working tree that holds `cwd`: its folder name, or "main checkout".
pub fn label(git: &GitInfo, cwd: &Path) -> Option<String> {
    let folder = git.folder_of(cwd)?;
    if git.is_main(folder) {
        return Some("main checkout".into());
    }
    folder.file_name().map(|name| name.to_string_lossy().into_owned())
}

/// The repository's working trees as the top-bar menu lists them: the main checkout first, then the others by name.
pub fn folders(git: &GitInfo) -> Vec<&Path> {
    let main = git.main_checkout();
    let mut others: Vec<&Path> = std::iter::once(&git.root).chain(&git.worktrees).map(PathBuf::as_path).filter(|p| path_key(p) != path_key(main)).collect();
    others.sort_by_key(|p| path_key(p));
    std::iter::once(main).chain(others).collect()
}

/// Only the folders Claudiu made are removed from Claudiu: a worktree made anywhere else, or in `.claude/worktrees` by
/// an earlier version or by Claude Code, is the user's own.
pub fn removable(root: &Path, main_checkout: &Path, folder: &Path) -> bool {
    belongs_to(root, main_checkout, folder)
}

/// What removing a worktree throws away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Leftovers {
    /// Changed or new files, which git refuses to delete without `--force`.
    pub files: usize,
    /// Commits that no remote branch has.
    pub unpushed: usize,
}

impl Leftovers {
    pub fn any(self) -> bool {
        self.files > 0 || self.unpushed > 0
    }
}

/// A typed name that names an existing worktree folder switches into it; one that only an existing branch has is
/// refused, so every branch of a worktree Claudiu made is one Claudiu made and removal may delete it once merged.
pub fn check_unused(root: &Path, main_checkout: &Path, name: &str) -> Result<(), String> {
    if folder(root, main_checkout, name).exists() || !branch_exists(main_checkout, name) {
        Ok(())
    } else {
        Err(format!("A branch called {name} already exists. Pick another name."))
    }
}

/// For `generate_name`: the folder or a local branch of that name exists.
pub fn name_taken<'a>(root: &'a Path, main_checkout: &'a Path) -> impl Fn(&str) -> bool + 'a {
    let repo = Repository::open(main_checkout).ok();
    move |name| folder(root, main_checkout, name).exists() || repo.as_ref().is_some_and(|r| r.find_branch(name, git2::BranchType::Local).is_ok())
}

fn branch_exists(main_checkout: &Path, name: &str) -> bool {
    Repository::open(main_checkout).is_ok_and(|r| r.find_branch(name, git2::BranchType::Local).is_ok())
}

/// Make the worktree `folder` on a new branch named like the folder, started from `base`. Blocking, and fetches from
/// origin for `Base::Main`.
pub fn add(main_checkout: &Path, folder: &Path, base: Base) -> Result<(), String> {
    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("the worktree has no name")?;
    let path = folder.to_string_lossy();
    let start = match base {
        Base::Main if has_origin(main_checkout) => {
            git(main_checkout, &["fetch", "origin"], None)?;
            origin_default_branch(main_checkout)?
        }
        _ => "HEAD".into(),
    };
    git(main_checkout, &["worktree", "add", "--no-track", "-b", &name, "--", &path, &start], None)?;
    Ok(())
}

/// Changed and new files in a working tree, not counting anything under `.claude/`, which Claude Code keeps its own
/// files in. Blocking, and takes no `index.lock`.
pub fn uncommitted(folder: &Path) -> Result<usize, String> {
    let status = git(folder, &["--no-optional-locks", "status", "--porcelain"], None)?;
    Ok(status.lines().filter(|l| l.len() > 3 && !l[3..].trim_start_matches('"').starts_with(".claude/")).count())
}

/// Blocking. Does not take `index.lock`, since Claude may be running git in the same folder.
pub fn leftovers(folder: &Path) -> Result<Leftovers, String> {
    let files = git(folder, &["--no-optional-locks", "status", "--porcelain"], None)?.lines().filter(|l| !l.trim().is_empty()).count();
    let unpushed = git(folder, &["rev-list", "--count", "HEAD", "--not", "--remotes"], None)?.trim().parse().unwrap_or(0);
    Ok(Leftovers { files, unpushed })
}

/// Remove the worktree, and its branch when that is fully merged. `discard` lets git delete changed and new files.
/// A Claude session that was just ended, and the programs it started, get `grace` to exit before they count as still
/// running. Returns a line for a toast. Blocking.
pub fn remove(root: &Path, main_checkout: &Path, folder: &Path, discard: bool, grace: std::time::Duration) -> Result<String, String> {
    if !removable(root, main_checkout, folder) {
        return Err(format!("{} was not made by Claudiu, so Claudiu leaves it alone.", folder.display()));
    }
    let deadline = std::time::Instant::now() + grace;
    let name = folder.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let branch = git(folder, &["branch", "--show-current"], None).ok().map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    let listing = git(main_checkout, &["worktree", "list", "--porcelain"], None)?;
    // A session that was ended leaves its lock behind. A lock set by anything else stays, and git then refuses.
    if let Some(reason) = lock_reason(&listing, folder).filter(|r| r.starts_with("claude session")) {
        if lock_pid(&reason).is_none_or(|pid| still_running(pid, grace)) {
            return Err(format!("A Claude session is still running in {name} ({reason})."));
        }
        git(main_checkout, &["worktree", "unlock", "--", &folder.to_string_lossy()], None)?;
    }
    let folder_in_use = delete_worktree(main_checkout, folder, discard, deadline)?;
    let mut message = match branch {
        None => format!("Removed worktree {name}."),
        Some(branch) => match git(main_checkout, &["branch", "-d", "--", &branch], None) {
            Ok(_) => format!("Removed worktree {name} and its merged branch {branch}."),
            Err(_) => format!("Removed worktree {name}. The branch {branch} is kept: it has commits that are not merged."),
        },
    };
    if folder_in_use {
        message.push_str(" Its folder is still in use by a program, such as a terminal or a tool Claude started. Delete it once that ends.");
    }
    Ok(message)
}

/// Delete a worktree folder that git no longer lists, or whose repository is gone, so `git worktree remove` can't.
/// Only a folder Claudiu made, two levels under `root`, is deleted. Blocking.
pub fn delete_folder(root: &Path, folder: &Path, grace: std::time::Duration) -> Result<String, String> {
    let made_here = folder.parent().and_then(Path::parent).is_some_and(|dir| path_key(dir) == path_key(root));
    if !made_here {
        return Err(format!("{} was not made by Claudiu, so Claudiu leaves it alone.", folder.display()));
    }
    if delete_leftover(folder, std::time::Instant::now() + grace) {
        Ok(format!("Deleted {}.", folder.display()))
    } else {
        Err(format!("{} is still in use by a program. Delete it once that ends.", folder.display()))
    }
}

/// `git worktree remove`, tried again while programs that are shutting down still hold something in the folder. On
/// Windows git deletes the files and the registration first and the folder last, so a program running in the folder
/// makes it fail with "Permission denied" although the worktree is gone; then only the leftover folder is retried.
/// Returns whether that folder could not be deleted before the deadline.
fn delete_worktree(main_checkout: &Path, folder: &Path, discard: bool, deadline: std::time::Instant) -> Result<bool, String> {
    let folder_arg = folder.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if discard {
        args.push("--force");
    }
    args.extend(["--", &folder_arg]);
    loop {
        let Err(failure) = git(main_checkout, &args, None) else { return Ok(false) };
        let listing = git(main_checkout, &["worktree", "list", "--porcelain"], None).unwrap_or_default();
        if block_of(&listing, folder).is_none() {
            return Ok(!delete_leftover(folder, deadline));
        }
        if !is_busy(&failure) || std::time::Instant::now() >= deadline {
            return Err(failure);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Delete what git left of the folder; false when something still holds it at the deadline.
fn delete_leftover(folder: &Path, deadline: std::time::Instant) -> bool {
    loop {
        if !folder.exists() || std::fs::remove_dir_all(folder).is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// The messages git and Windows give for a file or folder that a running program holds.
fn is_busy(failure: &str) -> bool {
    ["Permission denied", "being used by another process", "busy"].iter().any(|m| failure.contains(m))
}

fn still_running(pid: u32, grace: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    while crate::platform::process_running(pid) {
        if std::time::Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// The part of `git worktree list --porcelain` output that describes the worktree in `folder`, if git lists it.
fn block_of<'a>(porcelain: &'a str, folder: &Path) -> Option<&'a str> {
    porcelain.split("\n\n").find(|block| {
        block.lines().next().and_then(|line| line.strip_prefix("worktree ")).is_some_and(|path| path_key(Path::new(path)) == path_key(folder))
    })
}

/// The reason a worktree is locked (empty when none was given) in `git worktree list --porcelain` output.
fn lock_reason(porcelain: &str, folder: &Path) -> Option<String> {
    let line = block_of(porcelain, folder)?.lines().find(|l| *l == "locked" || l.starts_with("locked "))?;
    Some(line.strip_prefix("locked").unwrap_or_default().trim().to_string())
}

/// The process id in Claude Code's lock reason, `claude session <name> (pid <n>)`.
fn lock_pid(reason: &str) -> Option<u32> {
    reason.rsplit_once("(pid ")?.1.strip_suffix(')')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_WAIT: std::time::Duration = std::time::Duration::ZERO;

    #[test]
    fn names_allow_letters_digits_dash_underscore() {
        for ok in ["fix-login", "Fix_Login2", "a"] {
            assert_eq!(check_name(ok), Ok(()), "{ok}");
        }
        for bad in ["", "fix login", "feature/x", "v1.2", "naïve", "-x"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn generated_names_are_two_words_and_skip_taken_ones() {
        let first = generate_name(0, |_| false);
        assert_eq!(first, "bright-badger");
        assert!(check_name(&first).is_ok());
        assert_eq!(generate_name(0, |n| n == "bright-badger"), "bright-crane");
        assert_eq!(generate_name(0, |_| true), "worktree-0");
    }

    #[test]
    fn a_worktree_goes_under_the_root_in_a_folder_per_repository() {
        let root = Path::new("/d/worktrees");
        // The hash is FNV-1a of the repository path, worked out apart from this code.
        assert_eq!(folder(root, Path::new("/p/app"), "fix"), Path::new("/d/worktrees/app-2908e76a/fix"));
        // Two repositories of one name get two folders.
        assert_ne!(folder(root, Path::new("/q/app"), "fix"), folder(root, Path::new("/p/app"), "fix"));
        // The hash is the last dash-separated part, so a name with dashes keeps them.
        assert_eq!(repo_name("app-2908e76a"), "app");
        assert_eq!(repo_name("my-app-2908e76a"), "my-app");
    }

    #[test]
    fn the_root_lists_every_worktree_folder_by_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for dir in ["b-00000001/two", "b-00000001/one", "a-00000002/x"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("a-00000002/file.txt"), "not a worktree").unwrap();
        let listed: Vec<(String, String)> = made(root).into_iter().map(|m| (m.repo, m.folder.file_name().unwrap().to_string_lossy().into_owned())).collect();
        assert_eq!(listed, [("a-00000002", "x"), ("b-00000001", "one"), ("b-00000001", "two")].map(|(r, f)| (r.to_string(), f.to_string())));
        assert!(made(&root.join("missing")).is_empty());
    }

    #[test]
    fn a_folder_is_deleted_only_two_levels_under_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("worktrees");
        let ours = root.join("app-00000001/gone");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::write(ours.join("f.txt"), "x").unwrap();
        let theirs = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&theirs).unwrap();
        for refused in [&theirs, &root, &root.join("app-00000001")] {
            assert!(delete_folder(&root, refused, NO_WAIT).unwrap_err().contains("was not made by Claudiu"), "{refused:?}");
            assert!(refused.exists());
        }
        delete_folder(&root, &ours, NO_WAIT).unwrap();
        assert!(!ours.exists() && root.join("app-00000001").exists());
    }

    fn info(root: &str, main: &str, worktrees: &[&str], unborn: bool) -> GitInfo {
        GitInfo {
            root: root.into(),
            main: main.into(),
            worktrees: worktrees.iter().map(PathBuf::from).collect(),
            is_worktree: root != main,
            unborn,
            ..Default::default()
        }
    }

    #[test]
    fn a_worktree_needs_a_repository_with_a_commit() {
        assert_eq!(source(None), Err("is not a git repository"));
        assert_eq!(source(Some(&info("/p", "/p", &[], true))), Err("has no commits yet"));
        assert_eq!(source(Some(&info("/p", "/p", &[], false))), Ok(Path::new("/p")));
        // A project added from inside a linked worktree still branches from the main checkout.
        assert_eq!(source(Some(&info("/p/.claude/worktrees/a", "/p", &["/p"], false))), Ok(Path::new("/p")));
    }

    #[test]
    fn the_pill_names_the_working_tree_a_session_is_in() {
        let git = info("/p", "/p", &["/p/.claude/worktrees/fix"], false);
        assert_eq!(label(&git, Path::new("/p/src")).as_deref(), Some("main checkout"));
        assert_eq!(label(&git, Path::new("/p/.claude/worktrees/fix/src")).as_deref(), Some("fix"));
        assert_eq!(label(&git, Path::new("/elsewhere")), None);
        // A project added from inside a linked worktree: the checkout around it is still the main one.
        let linked = info("/p/.claude/worktrees/fix", "/p", &["/p"], false);
        assert_eq!(label(&linked, Path::new("/p/.claude/worktrees/fix")).as_deref(), Some("fix"));
        assert_eq!(label(&linked, Path::new("/p")).as_deref(), Some("main checkout"));
    }

    #[test]
    fn folders_list_the_main_checkout_first_then_the_rest_by_name() {
        let from_main = info("/p", "/p", &["/p/.claude/worktrees/zed", "/p/.claude/worktrees/ann"], false);
        assert_eq!(folders(&from_main), ["/p", "/p/.claude/worktrees/ann", "/p/.claude/worktrees/zed"].map(Path::new));

        let from_linked = info("/p/.claude/worktrees/ann", "/p", &["/p", "/p/.claude/worktrees/zed"], false);
        assert_eq!(folders(&from_linked), ["/p", "/p/.claude/worktrees/ann", "/p/.claude/worktrees/zed"].map(Path::new));
    }

    #[test]
    fn only_the_folders_claudiu_made_are_removable() {
        let (root, main) = (Path::new("/d/worktrees"), Path::new("/p/app"));
        assert!(removable(root, main, &folder(root, main, "fix")));
        assert!(!removable(root, main, main), "the main checkout");
        assert!(!removable(root, main, Path::new("/p/app-feature")), "a worktree made elsewhere");
        assert!(!removable(root, main, Path::new("/p/app/.claude/worktrees/fix")), "one made by Claude Code or an earlier version");
        assert!(!removable(root, main, &folder(root, main, "fix").join("src")), "a folder inside one");
        assert!(!removable(root, main, &folder(root, Path::new("/q/app"), "fix")), "another repository's");
    }

    #[test]
    fn lock_reasons_are_read_from_the_listing() {
        let listing = "worktree /p\nHEAD a\nbranch refs/heads/main\n\nworktree /p/.claude/worktrees/x\nHEAD b\nbranch refs/heads/x\nlocked claude session x (pid 4242)\n\nworktree /p/.claude/worktrees/y\nHEAD c\nbranch refs/heads/y\nlocked\n\nworktree /p/.claude/worktrees/z\nHEAD d\nbranch refs/heads/z\n";
        let reason = |folder: &str| lock_reason(listing, Path::new(folder));
        assert_eq!(reason("/p/.claude/worktrees/x").as_deref(), Some("claude session x (pid 4242)"));
        assert_eq!(reason("/p/.claude/worktrees/y").as_deref(), Some(""));
        assert_eq!(reason("/p/.claude/worktrees/z"), None);
        assert_eq!(reason("/p/.claude/worktrees/none"), None);
        assert_eq!(lock_pid("claude session x (pid 4242)"), Some(4242));
        assert_eq!(lock_pid("claude session x"), None);
    }

    /// Where a test keeps the worktrees it makes, in place of Claudiu's data folder.
    fn root_in(tmp: &Path) -> PathBuf {
        dunce::canonicalize(tmp).unwrap().join("data")
    }

    /// A repository with a commit and a worktree `fix` in the worktrees folder, made with the real `git`.
    fn repo_with_worktree(tmp: &Path) -> (PathBuf, PathBuf) {
        let main = dunce::canonicalize(tmp).unwrap().join("main");
        std::fs::create_dir_all(&main).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "one"]] {
            git(&main, args, None).unwrap();
        }
        let folder = folder(&root_in(tmp), &main, "fix");
        git(&main, &["worktree", "add", "-q", "-b", "worktree-fix", &folder.to_string_lossy()], None).unwrap();
        (main, folder)
    }

    #[test]
    fn a_new_worktree_gets_a_new_branch_named_like_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, _) = repo_with_worktree(tmp.path());
        git(&main, &["commit", "-q", "--allow-empty", "-m", "two"], None).unwrap();
        // No origin: both bases start from the current commit.
        let made = folder(&root_in(tmp.path()), &main, "fix-login");
        add(&main, &made, Base::Main).unwrap();
        assert_eq!(git(&made, &["branch", "--show-current"], None).unwrap().trim(), "fix-login");
        assert_eq!(git(&made, &["rev-parse", "HEAD"], None).unwrap(), git(&main, &["rev-parse", "HEAD"], None).unwrap());
        // A branch of that name already exists.
        git(&main, &["branch", "kept"], None).unwrap();
        assert!(add(&main, &folder(&root_in(tmp.path()), &main, "kept"), Base::Current).is_err());
    }

    #[test]
    fn a_name_only_an_existing_branch_has_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, _) = repo_with_worktree(tmp.path());
        git(&main, &["branch", "kept"], None).unwrap();
        assert_eq!(check_unused(&root_in(tmp.path()), &main, "new-one"), Ok(()));
        assert_eq!(check_unused(&root_in(tmp.path()), &main, "kept"), Err("A branch called kept already exists. Pick another name.".into()));
        // The worktree `fix` exists, on its branch `worktree-fix`: typing its folder name switches into it.
        assert_eq!(check_unused(&root_in(tmp.path()), &main, "fix"), Ok(()));
        let root = root_in(tmp.path());
        let taken = name_taken(&root, &main);
        assert!(taken("kept") && taken("fix") && taken("main") && !taken("new-one"));
    }

    #[test]
    fn leftovers_count_changed_files_and_commits_no_remote_has() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, folder) = repo_with_worktree(tmp.path());
        // The commit the branch started from is on no remote either, since this repository has none.
        assert_eq!(leftovers(&folder).unwrap(), Leftovers { files: 0, unpushed: 1 });
        std::fs::write(folder.join("a.txt"), "a").unwrap();
        std::fs::write(folder.join("b.txt"), "b").unwrap();
        assert_eq!(leftovers(&folder).unwrap().files, 2);
    }

    #[test]
    fn the_uncommitted_count_leaves_out_claudes_own_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, folder) = repo_with_worktree(tmp.path());
        assert_eq!(uncommitted(&main).unwrap(), 0);
        // What Claude Code leaves in a checkout: its settings.
        std::fs::create_dir_all(main.join(".claude")).unwrap();
        std::fs::write(main.join(".claude/settings.local.json"), "{}").unwrap();
        assert_eq!(uncommitted(&main).unwrap(), 0, ".claude/ is not the user's work");
        std::fs::write(main.join("mine.txt"), "x").unwrap();
        assert_eq!(uncommitted(&main).unwrap(), 1);
        std::fs::write(folder.join("a.txt"), "a").unwrap();
        assert_eq!(uncommitted(&folder).unwrap(), 1);
    }

    #[test]
    fn a_merged_worktree_goes_with_its_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, folder) = repo_with_worktree(tmp.path());
        let message = remove(&root_in(tmp.path()), &main, &folder, false, NO_WAIT).unwrap();
        assert!(message.contains("merged branch worktree-fix"), "{message}");
        assert!(!folder.exists());
        assert!(git(&main, &["rev-parse", "--verify", "-q", "worktree-fix"], None).is_err(), "the branch is gone");
    }

    #[test]
    fn unmerged_commits_keep_the_branch_and_changed_files_need_discard() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, folder) = repo_with_worktree(tmp.path());
        git(&folder, &["commit", "-q", "--allow-empty", "-m", "work"], None).unwrap();
        std::fs::write(folder.join("new.txt"), "x").unwrap();
        let refused = remove(&root_in(tmp.path()), &main, &folder, false, NO_WAIT).unwrap_err();
        assert!(refused.contains("untracked") || refused.contains("modified"), "{refused}");
        assert!(folder.exists());

        let message = remove(&root_in(tmp.path()), &main, &folder, true, NO_WAIT).unwrap();
        assert!(message.contains("branch worktree-fix is kept"), "{message}");
        assert!(!folder.exists());
        assert!(git(&main, &["rev-parse", "--verify", "-q", "worktree-fix"], None).is_ok(), "the commits are still reachable");
    }

    #[test]
    fn a_lock_left_by_an_ended_claude_session_is_released_but_a_live_one_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, folder) = repo_with_worktree(tmp.path());
        let lock = |pid: u32| git(&main, &["worktree", "lock", "--reason", &format!("claude session fix (pid {pid})"), &folder.to_string_lossy()], None).unwrap();

        lock(std::process::id());
        let refused = remove(&root_in(tmp.path()), &main, &folder, false, NO_WAIT).unwrap_err();
        assert!(refused.contains("still running"), "{refused}");
        assert!(folder.exists());

        git(&main, &["worktree", "unlock", &folder.to_string_lossy()], None).unwrap();
        lock(u32::MAX - 1);
        remove(&root_in(tmp.path()), &main, &folder, false, NO_WAIT).unwrap();
        assert!(!folder.exists());
    }

    /// A program running in `folder`, which on Windows keeps the folder from being deleted.
    #[cfg(windows)]
    fn run_in(folder: &Path, seconds: u32) -> std::process::Child {
        let mut cmd = std::process::Command::new("ping");
        cmd.args(["-n", &seconds.to_string(), "127.0.0.1"]).current_dir(folder).stdout(std::process::Stdio::null());
        crate::platform::no_window(&mut cmd);
        cmd.spawn().unwrap()
    }

    #[test]
    #[cfg(windows)]
    fn a_program_in_the_folder_leaves_the_folder_but_not_the_worktree_or_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, folder) = repo_with_worktree(tmp.path());
        let mut holder = run_in(&folder, 60);
        let message = remove(&root_in(tmp.path()), &main, &folder, false, std::time::Duration::from_millis(500)).unwrap();
        assert!(message.contains("merged branch worktree-fix") && message.contains("still in use"), "{message}");
        let listing = git(&main, &["worktree", "list", "--porcelain"], None).unwrap();
        assert!(block_of(&listing, &folder).is_none(), "git no longer lists it");
        holder.kill().unwrap();
        holder.wait().unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn a_program_that_exits_within_the_grace_does_not_leave_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, folder) = repo_with_worktree(tmp.path());
        let mut holder = run_in(&folder, 3);
        let message = remove(&root_in(tmp.path()), &main, &folder, false, std::time::Duration::from_secs(15)).unwrap();
        assert!(!message.contains("still in use") && !folder.exists(), "{message}");
        holder.wait().unwrap();
    }

    #[test]
    fn only_claudes_folders_are_ever_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, _) = repo_with_worktree(tmp.path());
        assert!(remove(&root_in(tmp.path()), &main, &main, true, NO_WAIT).unwrap_err().contains("was not made by Claudiu"));
        assert!(main.exists());
    }

    #[test]
    fn someone_elses_lock_stays_and_git_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, folder) = repo_with_worktree(tmp.path());
        git(&main, &["worktree", "lock", "--reason", "on a usb stick", &folder.to_string_lossy()], None).unwrap();
        assert!(remove(&root_in(tmp.path()), &main, &folder, true, NO_WAIT).unwrap_err().contains("locked"));
        assert!(folder.exists());
    }
}
