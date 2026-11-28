//! Managed `SKILL.md` packages: canonical storage inside m4code's data folder, global and per-project toggles, and
//! symlink syncing into each active harness's expected skills folder. No GPUI here.

pub mod registry;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::harness::subagents::CustomSubagents;
use crate::harness::Harness;
use crate::platform::{is_same_or_under, link_dir, remove_dir_link};

/// One installed skill package stored under `skills_root()/<name>/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSkill {
    /// Folder name in `skills_root()` and in each harness's `skills/` directory.
    pub name: String,
    /// `owner/repo` on GitHub.
    pub source: String,
    /// Identifier on `skills.sh` (often equal to `name`).
    pub skill_id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub installed_at: i64,
}

impl InstalledSkill {
    /// The catalog entry that installs this skill again, so updating reuses the install path and lands in the same
    /// folder (`folder_name()` prefers `name`, which is already a valid folder name).
    pub fn catalog_entry(&self) -> registry::CatalogEntry {
        registry::CatalogEntry {
            source: self.source.clone(),
            skill_id: self.skill_id.clone(),
            name: self.name.clone(),
            installs: 0,
            is_official: false,
        }
    }
}

/// A skill the project's repository ships itself, such as `.claude/skills/<name>/SKILL.md`. m4code only lists these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSkill {
    pub name: String,
    pub description: String,
    /// Skills folder relative to the project root, such as `.claude/skills`.
    pub folder: PathBuf,
}

/// Persisted skill state (stored in `Settings.skills`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsConfig {
    #[serde(default)]
    pub installed: Vec<InstalledSkill>,
    /// Skill names enabled globally across active harnesses.
    #[serde(default)]
    pub global_enabled: Vec<String>,
    /// Skill names enabled per project, keyed by `Project.id`.
    #[serde(default)]
    pub project_enabled: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogStatus<'a> {
    Available,
    Installed,
    NameInUse(&'a str),
}

pub fn catalog_status<'a>(installed: &'a [InstalledSkill], entry: &registry::CatalogEntry) -> CatalogStatus<'a> {
    if installed.iter().any(|skill| skill.source.eq_ignore_ascii_case(&entry.source) && skill.skill_id == entry.skill_id) {
        return CatalogStatus::Installed;
    }
    if let Some(folder) = entry.folder_name()
        && let Some(skill) = installed.iter().find(|skill| skill.name.eq_ignore_ascii_case(folder))
    {
        return CatalogStatus::NameInUse(&skill.source);
    }
    CatalogStatus::Available
}

impl SkillsConfig {
    pub fn catalog_status(&self, entry: &registry::CatalogEntry) -> CatalogStatus<'_> {
        catalog_status(&self.installed, entry)
    }

    pub fn is_installed(&self, name: &str) -> bool {
        self.installed.iter().any(|s| s.name == name)
    }

    pub fn is_global(&self, name: &str) -> bool {
        self.global_enabled.iter().any(|n| n == name)
    }

    pub fn is_project(&self, project_id: &str, name: &str) -> bool {
        self.project_enabled.get(project_id).is_some_and(|list| list.iter().any(|n| n == name))
    }

    /// Insert or update an installed skill record, sorted alphabetically by name.
    pub fn upsert(&mut self, skill: InstalledSkill) {
        if let Some(existing) = self.installed.iter_mut().find(|s| s.name == skill.name) {
            *existing = skill;
        } else {
            self.installed.push(skill);
            self.installed.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    pub fn set_global(&mut self, name: &str, on: bool) {
        set_flag(&mut self.global_enabled, name, on);
    }

    pub fn toggle_global(&mut self, name: &str) -> bool {
        toggle_flag(&mut self.global_enabled, name)
    }

    pub fn set_project(&mut self, project_id: &str, name: &str, on: bool) {
        let entry = self.project_enabled.entry(project_id.to_string()).or_default();
        set_flag(entry, name, on);
        if entry.is_empty() {
            self.project_enabled.remove(project_id);
        }
    }

    pub fn toggle_project(&mut self, project_id: &str, name: &str) -> bool {
        let entry = self.project_enabled.entry(project_id.to_string()).or_default();
        let on = toggle_flag(entry, name);
        if entry.is_empty() {
            self.project_enabled.remove(project_id);
        }
        on
    }

    /// Installed skills grouped by the GitHub `owner/repo` they came from, groups and skills sorted by name.
    pub fn by_source(&self) -> Vec<(&str, Vec<&InstalledSkill>)> {
        let mut groups: BTreeMap<&str, Vec<&InstalledSkill>> = BTreeMap::new();
        for skill in &self.installed {
            groups.entry(skill.source.as_str()).or_default().push(skill);
        }
        groups.into_iter().collect()
    }

    /// Remove an installed skill and every global or per-project toggle referencing it.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.installed.len();
        self.installed.retain(|s| s.name != name);
        self.global_enabled.retain(|n| n != name);
        for list in self.project_enabled.values_mut() {
            list.retain(|n| n != name);
        }
        self.project_enabled.retain(|_, list| !list.is_empty());
        self.installed.len() != before
    }
}

fn set_flag(list: &mut Vec<String>, name: &str, on: bool) {
    if on {
        if !list.iter().any(|n| n == name) {
            list.push(name.to_string());
            list.sort();
        }
    } else {
        list.retain(|n| n != name);
    }
}

fn toggle_flag(list: &mut Vec<String>, name: &str) -> bool {
    if let Some(pos) = list.iter().position(|n| n == name) {
        list.remove(pos);
        false
    } else {
        list.push(name.to_string());
        list.sort();
        true
    }
}

/// Canonical folder where m4code keeps downloaded `SKILL.md` packages. Storing them here (rather than directly inside
/// `~/.agents/skills`) lets turning a skill off hide it even from harnesses that auto-scan `~/.agents/skills`.
pub fn skills_root() -> PathBuf {
    crate::datadir::data_dir().join("skills")
}

/// Safe skill folder name: non-empty, ASCII alphanumeric plus `-`, `_`, `.`, starting with alphanumeric, no `..`.
pub fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Safe GitHub `owner/repo` string.
pub fn valid_source(source: &str) -> bool {
    let Some((owner, repo)) = source.split_once('/') else { return false };
    !repo.contains('/') && valid_repo_segment(owner) && valid_repo_segment(repo)
}

fn valid_repo_segment(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 100
        && part != "."
        && part != ".."
        && !part.contains("..")
        && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Extract `(name, description)` from a `SKILL.md`'s leading YAML frontmatter block (`--- ... ---`).
/// Handles single-line values (quoted or unquoted) and folded/literal multiline blocks (`>`, `>-`, `|`, `|-`).
pub fn parse_frontmatter(markdown: &str) -> (Option<String>, Option<String>) {
    let text = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let mut frontmatter: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            closed = true;
            break;
        }
        frontmatter.push(line);
    }
    if !closed {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    let mut i = 0;
    while i < frontmatter.len() {
        let line = frontmatter[i];
        i += 1;
        if line.starts_with(char::is_whitespace) || line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else { continue };
        let key = key.trim();
        let val = rest.trim();
        let parsed = if matches!(val, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
            let mut parts: Vec<&str> = Vec::new();
            while i < frontmatter.len() {
                let next = frontmatter[i];
                if next.starts_with(char::is_whitespace) {
                    let t = next.trim();
                    if !t.is_empty() {
                        parts.push(t);
                    }
                    i += 1;
                } else if next.trim().is_empty() {
                    i += 1;
                } else {
                    break;
                }
            }
            parts.join(" ")
        } else {
            unquote_yaml(val).to_string()
        };
        if parsed.is_empty() {
            continue;
        }
        match key {
            "name" => name = Some(parsed),
            "description" => description = Some(parsed),
            _ => {}
        }
    }
    (name, description)
}

fn unquote_yaml(raw: &str) -> &str {
    if raw.len() >= 2 && ((raw.starts_with('"') && raw.ends_with('"')) || (raw.starts_with('\'') && raw.ends_with('\''))) {
        &raw[1..raw.len() - 1]
    } else {
        raw
    }
}

/// Config directory whose presence indicates the harness is set up on this machine.
pub fn harness_config_root(harness: Harness, home: &Path) -> PathBuf {
    (harness.adapter().config_root)(home)
}

/// Global skills directory that `harness` reads automatically.
pub fn global_skills_dir(harness: Harness, home: &Path) -> PathBuf {
    harness_config_root(harness, home).join("skills")
}

/// Per-project skills directory that `harness` reads automatically.
pub fn project_skills_dir(harness: Harness, project: &Path) -> PathBuf {
    project.join(harness.adapter().project_skills)
}

/// Harnesses that should receive synced skills: every harness enabled in Settings → Harnesses, any harness turned on
/// as a subagent, any harness explicitly requested, and any harness whose config folder already exists on disk.
pub fn active_harnesses(enabled: &[Harness], default: Harness, subagents: &CustomSubagents, home: &Path, extra: Option<Harness>) -> Vec<Harness> {
    Harness::registered()
        .filter(|&h| {
            enabled.contains(&h)
                || h == default
                || extra == Some(h)
                || subagent_enabled(h, subagents)
                || harness_config_root(h, home).exists()
        })
        .collect()
}

fn subagent_enabled(harness: Harness, subagents: &CustomSubagents) -> bool {
    harness.agent().is_some_and(|agent| subagents.get(agent).on)
}

/// Marker file placed inside m4code-managed copied skill directories (used for harnesses whose discovery skips
/// symlinks/junctions, such as Antigravity). Stores the canonical `store_root` path so user-owned folders are never
/// mistaken for m4code-managed copies.
const MANAGED_COPY_MARKER: &str = ".m4code-skill-managed";

/// Whether `harness` discovers skills with a non-symlink-following directory scan (e.g. Go's `os.ReadDir` +
/// `entry.IsDir()`), requiring real copied directories rather than symlinks/junctions.
fn uses_copied_skills(harness: Harness) -> bool {
    harness.adapter().copies_skills
}

/// Sibling m4code app folder names under the shared vendor directory (`m4rocks/`) so dev and release builds
/// (and legacy `Claudiu` builds) recognize and migrate each other's managed skill links and copies.
const SIBLING_APP_DIRS: &[&str] = &["m4code", "m4code Dev", "Claudiu", "Claudiu Dev"];

/// Candidate canonical `skills/` roots owned by m4code: `store_root` itself, plus sibling dev/release `m4rocks/`
/// skill roots when `store_root` follows the standard `<vendor>/<app>/data/skills` layout.
fn managed_store_roots(store_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![store_root.to_path_buf()];
    if store_root.file_name().and_then(|n| n.to_str()) == Some("skills")
        && let Some(data_dir) = store_root.parent()
        && data_dir.file_name().and_then(|n| n.to_str()) == Some("data")
        && let Some(app_dir) = data_dir.parent()
        && let Some(app_name) = app_dir.file_name().and_then(|n| n.to_str())
        && SIBLING_APP_DIRS.contains(&app_name)
        && let Some(vendor_dir) = app_dir.parent()
    {
        for sibling in SIBLING_APP_DIRS {
            if *sibling != app_name {
                roots.push(vendor_dir.join(sibling).join("data").join("skills"));
            }
        }
    }
    roots
}

/// Whether `link` is a symlink/junction created by m4code (including sibling dev/release builds) that points inside
/// `store_root`. Also matches dangling links whose target path lies under a managed store root.
fn is_managed_link(link: &Path, store_root: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(link) else { return false };
    if !meta.file_type().is_symlink() {
        return false;
    }
    let Ok(target) = fs::read_link(link) else { return false };
    let resolved = if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or(Path::new("")).join(target)
    };
    let canon_resolved = fs::canonicalize(&resolved).ok();
    for root in managed_store_roots(store_root) {
        if is_same_or_under(&resolved, &root) {
            return true;
        }
        if let (Some(r), Ok(s)) = (&canon_resolved, fs::canonicalize(&root))
            && is_same_or_under(r, &s)
        {
            return true;
        }
    }
    false
}

/// Whether `dir` is a real directory copied and managed by m4code (including sibling dev/release builds).
fn is_managed_copy(dir: &Path, store_root: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(dir) else { return false };
    if !meta.file_type().is_dir() {
        return false;
    }
    let Ok(recorded) = fs::read_to_string(dir.join(MANAGED_COPY_MARKER)) else { return false };
    let recorded = PathBuf::from(recorded.trim());
    let canon_recorded = fs::canonicalize(&recorded).ok();
    for root in managed_store_roots(store_root) {
        if is_same_or_under(&recorded, &root) && is_same_or_under(&root, &recorded) {
            return true;
        }
        if let (Some(r), Ok(s)) = (&canon_recorded, fs::canonicalize(&root))
            && r == &s
        {
            return true;
        }
    }
    false
}

fn is_managed_entry(path: &Path, store_root: &Path) -> bool {
    is_managed_link(path, store_root) || is_managed_copy(path, store_root)
}

fn remove_managed_entry(path: &Path, store_root: &Path) -> io::Result<()> {
    if is_managed_link(path, store_root) {
        remove_dir_link(path)
    } else if is_managed_copy(path, store_root) {
        fs::remove_dir_all(path)
    } else {
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" || name == MANAGED_COPY_MARKER {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn write_managed_copy(src: &Path, dst: &Path, store_root: &Path) -> io::Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    copy_dir_recursive(src, dst)?;
    fs::write(dst.join(MANAGED_COPY_MARKER), store_root.to_string_lossy().as_bytes())
}

/// Reconcile `target_dir` using real copied directories (migrating any legacy managed symlinks/junctions).
/// User-owned folders without `MANAGED_COPY_MARKER` are never touched.
pub fn sync_dir_copied(store_root: &Path, target_dir: &Path, enabled: &[String]) -> io::Result<()> {
    let mut last_err = None;
    if target_dir.is_dir() {
        for entry in fs::read_dir(target_dir)?.flatten() {
            let path = entry.path();
            if is_managed_link(&path, store_root) {
                // Migrate legacy symlinks/junctions so they are replaced with real copies if enabled.
                if let Err(e) = remove_dir_link(&path) {
                    last_err = Some(e);
                }
                continue;
            }
            if !is_managed_copy(&path, store_root) {
                continue;
            }
            let name = entry.file_name();
            let keep = name.to_str().is_some_and(|n| {
                enabled.iter().any(|e| e == n) && store_root.join(n).join("SKILL.md").is_file()
            });
            if !keep && let Err(e) = fs::remove_dir_all(&path) {
                last_err = Some(e);
            }
        }
    }
    if enabled.is_empty() {
        return match last_err {
            Some(e) => Err(e),
            None => Ok(()),
        };
    }
    for name in enabled {
        if !valid_skill_name(name) {
            continue;
        }
        let src = store_root.join(name);
        if !src.join("SKILL.md").is_file() {
            continue;
        }
        if let Err(e) = fs::create_dir_all(target_dir) {
            last_err = Some(e);
            continue;
        }
        let dst = target_dir.join(name);
        if fs::symlink_metadata(&dst).is_ok() && !is_managed_copy(&dst, store_root) {
            // User-owned entry; never clobber.
            continue;
        }
        if let Err(e) = write_managed_copy(&src, &dst, store_root) {
            last_err = Some(e);
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn sync_dir_for_harness(harness: Harness, store_root: &Path, target_dir: &Path, enabled: &[String]) -> io::Result<()> {
    if uses_copied_skills(harness) {
        sync_dir_copied(store_root, target_dir, enabled)
    } else {
        sync_dir(store_root, target_dir, enabled)
    }
}

/// Reconcile `target_dir` so its m4code-managed symlinks match `enabled` exactly.
/// User-owned folders or links pointing outside `store_root` are never touched.
pub fn sync_dir(store_root: &Path, target_dir: &Path, enabled: &[String]) -> io::Result<()> {
    let mut last_err = None;
    if target_dir.is_dir() {
        for entry in fs::read_dir(target_dir)?.flatten() {
            let path = entry.path();
            if !is_managed_link(&path, store_root) {
                continue;
            }
            let name = entry.file_name();
            let keep = name.to_str().is_some_and(|n| {
                enabled.iter().any(|e| e == n) && store_root.join(n).join("SKILL.md").is_file()
            });
            if !keep && let Err(e) = remove_dir_link(&path) {
                last_err = Some(e);
            }
        }
    }
    if enabled.is_empty() {
        return match last_err {
            Some(e) => Err(e),
            None => Ok(()),
        };
    }
    for name in enabled {
        if !valid_skill_name(name) {
            continue;
        }
        let src = store_root.join(name);
        if !src.join("SKILL.md").is_file() {
            continue;
        }
        if let Err(e) = fs::create_dir_all(target_dir) {
            last_err = Some(e);
            continue;
        }
        let dst = target_dir.join(name);
        if fs::symlink_metadata(&dst).is_ok() {
            // Either already linked or user-owned; never clobber an existing entry.
            continue;
        }
        if let Err(e) = link_dir(&src, &dst) {
            last_err = Some(e);
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Track shared `.agents/skills` symlinks that m4code itself created so we never prune repo-owned symlinks.
fn shared_links_manifest(store_root: &Path) -> PathBuf {
    store_root.join(".shared-links.json")
}

fn load_shared_links(store_root: &Path) -> std::collections::HashSet<PathBuf> {
    fs::read_to_string(shared_links_manifest(store_root))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<PathBuf>>(&s).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn save_shared_links(store_root: &Path, links: &std::collections::HashSet<PathBuf>) {
    let mut sorted: Vec<&PathBuf> = links.iter().collect();
    sorted.sort();
    if let Ok(text) = serde_json::to_string(&sorted) {
        let _ = crate::datadir::write_atomic(&shared_links_manifest(store_root), &text);
    }
}

fn sync_shared_dir(shared_root: &Path, target_dir: &Path, wanted: &[String], managed: &mut std::collections::HashSet<PathBuf>) -> io::Result<()> {
    let mut last_err = None;
    if target_dir.is_dir() {
        for entry in fs::read_dir(target_dir)?.flatten() {
            let path = entry.path();
            if !managed.contains(&path) {
                continue;
            }
            let name = entry.file_name();
            let keep = name.to_str().is_some_and(|n| {
                wanted.iter().any(|e| e == n) && shared_root.join(n).join("SKILL.md").is_file()
            });
            if !keep {
                if let Err(e) = remove_dir_link(&path) {
                    last_err = Some(e);
                } else {
                    managed.remove(&path);
                }
            }
        }
    }
    for name in wanted {
        if !valid_skill_name(name) {
            continue;
        }
        let src = shared_root.join(name);
        if !src.join("SKILL.md").is_file() {
            continue;
        }
        if let Err(e) = fs::create_dir_all(target_dir) {
            last_err = Some(e);
            continue;
        }
        let dst = target_dir.join(name);
        if fs::symlink_metadata(&dst).is_ok() {
            continue;
        }
        match link_dir(&src, &dst) {
            Ok(()) => {
                managed.insert(dst);
            }
            Err(e) => last_err = Some(e),
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Sync global and per-project skills into every active harness (and prune stale m4code links from inactive ones).
/// Repo skills placed in the shared `.agents/skills` folder are also projected into other active harnesses' project
/// skill folders so every model sees the same project skills without duplicating files or pruning repo-owned symlinks.
/// Returns human-readable error strings for any directories that failed to sync.
pub fn sync_all(store_root: &Path, home: &Path, active: &[Harness], config: &SkillsConfig, projects: &[(String, PathBuf)]) -> Vec<String> {
    let mut errors = Vec::new();
    for harness in Harness::registered() {
        let dir = global_skills_dir(harness, home);
        let wanted = if active.contains(&harness) { config.global_enabled.as_slice() } else { &[] };
        if let Err(e) = sync_dir_for_harness(harness, store_root, &dir, wanted) {
            errors.push(format!("{} global skills: {e}", harness.name()));
        }
    }
    let mut managed_shared = load_shared_links(store_root);
    let initial_shared = managed_shared.clone();
    for (project_id, project_path) in projects {
        if !project_path.is_dir() {
            continue;
        }
        let project_wanted = config.project_enabled.get(project_id).map_or(&[][..], Vec::as_slice);
        let shared_root = project_path.join(".agents/skills");
        let shared_repo_skills: Vec<String> = if shared_root.is_dir() {
            fs::read_dir(&shared_root)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if is_managed_entry(&path, store_root) || !path.join("SKILL.md").is_file() {
                        return None;
                    }
                    entry.file_name().to_str().filter(|n| valid_skill_name(n)).map(str::to_string)
                })
                .collect()
        } else {
            Vec::new()
        };
        for harness in Harness::registered() {
            let dir = project_skills_dir(harness, project_path);
            let wanted = if active.contains(&harness) { project_wanted } else { &[] };
            if let Err(e) = sync_dir_for_harness(harness, store_root, &dir, wanted) {
                errors.push(format!("{} skills in {}: {e}", harness.name(), project_path.display()));
            }
            if dir != shared_root {
                let shared_wanted = if active.contains(&harness) { shared_repo_skills.as_slice() } else { &[] };
                if let Err(e) = sync_shared_dir(&shared_root, &dir, shared_wanted, &mut managed_shared) {
                    errors.push(format!("{} shared repo skills in {}: {e}", harness.name(), project_path.display()));
                }
            }
        }
    }
    if managed_shared != initial_shared {
        save_shared_links(store_root, &managed_shared);
    }
    errors
}

/// Skills the repository at `project` ships in any harness's project skills folder. m4code's own symlinks/copies from
/// `store_root` or `.agents/skills` are left out so each skill appears once.
pub fn repo_skills(store_root: &Path, project: &Path) -> Vec<RepoSkill> {
    let shared_root = project.join(".agents/skills");
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut dirs: Vec<PathBuf> = vec![shared_root.clone()];
    for harness in Harness::registered() {
        let dir = project_skills_dir(harness, project);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        let folder = dir.strip_prefix(project).unwrap_or(&dir).to_path_buf();
        for entry in entries.flatten() {
            let path = entry.path();
            if is_managed_entry(&path, store_root) || (dir != shared_root && is_managed_link(&path, &shared_root)) {
                continue;
            }
            let Ok(skill_md) = fs::read_to_string(path.join("SKILL.md")) else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !seen.insert(name.clone()) {
                continue;
            }
            let (_, description) = parse_frontmatter(&skill_md);
            found.push(RepoSkill {
                name,
                description: description.unwrap_or_default(),
                folder: folder.clone(),
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.folder.cmp(&b.folder)));
    found
}

/// Remove every m4code-managed link or copy for `name` (globally and in `projects`) and delete its folder in `store_root`.
pub fn uninstall(store_root: &Path, home: &Path, name: &str, projects: &[PathBuf]) -> io::Result<()> {
    if !valid_skill_name(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid skill name"));
    }
    for harness in Harness::registered() {
        let entry = global_skills_dir(harness, home).join(name);
        if is_managed_entry(&entry, store_root) {
            let _ = remove_managed_entry(&entry, store_root);
        }
    }
    for project in projects {
        for harness in Harness::registered() {
            let entry = project_skills_dir(harness, project).join(name);
            if is_managed_entry(&entry, store_root) {
                let _ = remove_managed_entry(&entry, store_root);
            }
        }
    }
    let dir = store_root.join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: Test skill\n---\n# {name}\n")).unwrap();
        dir
    }

    #[test]
    fn valid_names_and_sources_reject_traversal() {
        assert!(valid_skill_name("agent-browser"));
        assert!(valid_skill_name("my_skill.v2"));
        assert!(!valid_skill_name(""));
        assert!(!valid_skill_name(".."));
        assert!(!valid_skill_name("../etc"));
        assert!(!valid_skill_name("a/b"));
        assert!(!valid_skill_name(".hidden"));

        assert!(valid_source("vercel-labs/agent-browser"));
        assert!(valid_source("m4rocks/m4skills"));
        assert!(!valid_source("no-slash"));
        assert!(!valid_source("a/b/c"));
        assert!(!valid_source("../evil"));
        assert!(!valid_source("owner/.."));
    }

    #[test]
    fn parses_single_line_and_folded_yaml_frontmatter() {
        let single = "---\nname: \"agent-browser\"\ndescription: 'Browser automation CLI'\n---\n# Body";
        assert_eq!(parse_frontmatter(single), (Some("agent-browser".into()), Some("Browser automation CLI".into())));

        let folded = "---\nname: agy-guide\ndescription: >-\n  First line of help.\n  Second line.\n---\n# Body";
        assert_eq!(parse_frontmatter(folded), (Some("agy-guide".into()), Some("First line of help. Second line.".into())));

        assert_eq!(parse_frontmatter("# No frontmatter"), (None, None));
    }

    #[test]
    fn sync_links_only_active_harnesses_and_never_touches_user_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        write_skill(&store, "alpha");
        write_skill(&store, "beta");

        // Pre-existing user-owned skill in Claude's global dir must not be replaced or removed.
        let user_skill = global_skills_dir(Harness::Claude, &home).join("hand-made");
        fs::create_dir_all(&user_skill).unwrap();
        fs::write(user_skill.join("SKILL.md"), "user").unwrap();

        let mut cfg = SkillsConfig::default();
        cfg.set_global("alpha", true);
        cfg.set_project("p1", "beta", true);

        let active = vec![Harness::Claude, Harness::Antigravity];
        let errs = sync_all(&store, &home, &active, &cfg, &[("p1".into(), project.clone())]);
        assert!(errs.is_empty(), "{errs:?}");

        // Active harnesses get global alpha and project beta.
        assert!(global_skills_dir(Harness::Claude, &home).join("alpha").join("SKILL.md").is_file());
        assert!(global_skills_dir(Harness::Antigravity, &home).join("alpha").join("SKILL.md").is_file());
        assert!(project_skills_dir(Harness::Claude, &project).join("beta").join("SKILL.md").is_file());
        assert!(project_skills_dir(Harness::Antigravity, &project).join("beta").join("SKILL.md").is_file());
        // Inactive harnesses do not get directories created.
        assert!(!global_skills_dir(Harness::Codex, &home).exists());
        assert!(!project_skills_dir(Harness::Codex, &project).exists());
        // User-owned folder stays intact.
        assert_eq!(fs::read_to_string(user_skill.join("SKILL.md")).unwrap(), "user");

        // Toggling off removes m4code's symlink while keeping the canonical skill and user folder intact.
        cfg.set_global("alpha", false);
        let errs = sync_all(&store, &home, &active, &cfg, &[("p1".into(), project.clone())]);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(fs::symlink_metadata(global_skills_dir(Harness::Claude, &home).join("alpha")).is_err());
        assert!(fs::symlink_metadata(global_skills_dir(Harness::Antigravity, &home).join("alpha")).is_err());
        assert!(store.join("alpha").join("SKILL.md").is_file());
        assert!(user_skill.join("SKILL.md").is_file());

        // Uninstalling removes project symlinks and canonical folder.
        uninstall(&store, &home, "beta", std::slice::from_ref(&project)).unwrap();
        assert!(fs::symlink_metadata(project_skills_dir(Harness::Claude, &project).join("beta")).is_err());
        assert!(fs::symlink_metadata(project_skills_dir(Harness::Antigravity, &project).join("beta")).is_err());
        assert!(!store.join("beta").exists());
    }

    #[test]
    fn repo_skills_lists_project_folders_but_not_m4code_links() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        write_skill(&store, "installed");
        write_skill(&project_skills_dir(Harness::Claude, &project), "zeta");
        write_skill(&project_skills_dir(Harness::Antigravity, &project), "alpha");
        // A folder without SKILL.md is not a skill.
        fs::create_dir_all(project_skills_dir(Harness::Claude, &project).join("notes")).unwrap();

        let mut cfg = SkillsConfig::default();
        cfg.set_project("p1", "installed", true);
        let errs = sync_all(&store, &home, &[Harness::Claude], &cfg, &[("p1".into(), project.clone())]);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(project_skills_dir(Harness::Claude, &project).join("installed").join("SKILL.md").is_file());

        let found = repo_skills(&store, &project);
        let listed: Vec<(&str, PathBuf)> = found.iter().map(|s| (s.name.as_str(), s.folder.clone())).collect();
        assert_eq!(listed, vec![("alpha", PathBuf::from(".agents/skills")), ("zeta", PathBuf::from(".claude/skills"))]);
        assert_eq!(found[0].description, "Test skill");
    }

    #[test]
    fn update_entry_reinstalls_into_the_same_folder() {
        let skill = InstalledSkill {
            name: "agent-browser".into(),
            source: "vercel-labs/agent-browser".into(),
            skill_id: "Agent Browser".into(),
            description: String::new(),
            installed_at: 0,
        };
        let entry = skill.catalog_entry();
        assert_eq!(entry.source, skill.source);
        assert_eq!(entry.skill_id, skill.skill_id);
        assert_eq!(entry.folder_name(), Some("agent-browser"));
    }

    #[test]
    fn catalogue_status_distinguishes_identically_named_skills_by_author() {
        let mut config = SkillsConfig::default();
        config.upsert(InstalledSkill { name: "review".into(), source: "alice/skills".into(), skill_id: "review".into(), description: String::new(), installed_at: 1 });
        let entry = registry::CatalogEntry { name: "review".into(), source: "bob/skills".into(), skill_id: "review".into(), installs: 0, is_official: false };
        assert_eq!(config.catalog_status(&entry), CatalogStatus::NameInUse("alice/skills"));
        assert_eq!(config.catalog_status(&registry::CatalogEntry { source: "Alice/Skills".into(), ..entry.clone() }), CatalogStatus::Installed);
        assert_eq!(config.catalog_status(&registry::CatalogEntry { name: "audit".into(), ..entry }), CatalogStatus::Available);
    }

    #[test]
    fn antigravity_uses_real_copied_dirs_migrates_legacy_links_and_keeps_user_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        write_skill(&store, "autonomy");

        // Pre-populate a legacy junction/symlink in Antigravity's global skills folder.
        let agy_global = global_skills_dir(Harness::Antigravity, &home);
        fs::create_dir_all(&agy_global).unwrap();
        let legacy_link = agy_global.join("autonomy");
        link_dir(&store.join("autonomy"), &legacy_link).unwrap();
        assert!(fs::symlink_metadata(&legacy_link).unwrap().file_type().is_symlink());

        // Pre-populate a user-owned directory that must never be clobbered.
        let user_dir = agy_global.join("custom");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(user_dir.join("SKILL.md"), "user-owned").unwrap();

        let mut cfg = SkillsConfig::default();
        cfg.set_global("autonomy", true);
        cfg.set_project("p1", "autonomy", true);
        let errs = sync_all(&store, &home, &[Harness::Antigravity], &cfg, &[("p1".into(), project.clone())]);
        assert!(errs.is_empty(), "{errs:?}");

        // Legacy link was migrated to a real directory (not a symlink/junction) with the managed marker.
        let global_meta = fs::symlink_metadata(&legacy_link).unwrap();
        assert!(global_meta.file_type().is_dir() && !global_meta.file_type().is_symlink());
        assert!(legacy_link.join("SKILL.md").is_file());
        assert!(is_managed_copy(&legacy_link, &store));

        // Project copy in .agents/skills is also a real directory and excluded from repo_skills().
        let proj_copy = project_skills_dir(Harness::Antigravity, &project).join("autonomy");
        let proj_meta = fs::symlink_metadata(&proj_copy).unwrap();
        assert!(proj_meta.file_type().is_dir() && !proj_meta.file_type().is_symlink());
        assert!(repo_skills(&store, &project).is_empty());

        // Updating the canonical skill refreshes the managed copy on next sync.
        fs::write(store.join("autonomy").join("SKILL.md"), "---\nname: autonomy\ndescription: Updated\n---\n").unwrap();
        let errs = sync_all(&store, &home, &[Harness::Antigravity], &cfg, &[("p1".into(), project.clone())]);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(fs::read_to_string(legacy_link.join("SKILL.md")).unwrap().contains("Updated"));

        // User-owned directory stays untouched.
        assert_eq!(fs::read_to_string(user_dir.join("SKILL.md")).unwrap(), "user-owned");
    }

    #[test]
    fn dev_build_migrates_release_build_antigravity_junctions_to_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let vendor = tmp.path().join("m4rocks");
        let release_store = vendor.join("m4code").join("data").join("skills");
        let dev_store = vendor.join("m4code Dev").join("data").join("skills");
        let home = tmp.path().join("home");
        write_skill(&release_store, "autonomy");
        write_skill(&dev_store, "autonomy");

        let agy_global = global_skills_dir(Harness::Antigravity, &home);
        fs::create_dir_all(&agy_global).unwrap();
        let legacy_link = agy_global.join("autonomy");
        link_dir(&release_store.join("autonomy"), &legacy_link).unwrap();
        assert!(fs::symlink_metadata(&legacy_link).unwrap().file_type().is_symlink());

        let mut cfg = SkillsConfig::default();
        cfg.set_global("autonomy", true);
        let errs = sync_all(&dev_store, &home, &[Harness::Antigravity], &cfg, &[]);
        assert!(errs.is_empty(), "{errs:?}");

        let meta = fs::symlink_metadata(&legacy_link).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        assert!(is_managed_copy(&legacy_link, &dev_store));
        assert!(is_managed_copy(&legacy_link, &release_store));
    }

    #[test]
    fn by_source_groups_installed_skills_by_repo() {
        let mut cfg = SkillsConfig::default();
        for (name, source) in [("b", "z/repo"), ("a", "z/repo"), ("c", "a/repo")] {
            cfg.upsert(InstalledSkill { name: name.into(), source: source.into(), skill_id: name.into(), description: String::new(), installed_at: 0 });
        }
        let groups: Vec<(&str, Vec<&str>)> = cfg.by_source().into_iter().map(|(src, list)| (src, list.iter().map(|s| s.name.as_str()).collect())).collect();
        assert_eq!(groups, vec![("a/repo", vec!["c"]), ("z/repo", vec!["a", "b"])]);
    }
}
