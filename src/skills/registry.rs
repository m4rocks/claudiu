//! Client for the `skills.sh` directory and installer for `SKILL.md` packages hosted on GitHub.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use super::{InstalledSkill, parse_frontmatter, valid_skill_name, valid_source};

const SKILLS_SH_POPULAR: &str = "https://skills.sh/api/skills/all-time/0";
const SKILLS_SH_SEARCH: &str = "https://skills.sh/api/search";
const MAX_FILES_PER_SKILL: usize = 100;
const MAX_BYTES_PER_FILE: usize = 5 * 1024 * 1024;

/// A skill entry listed on `skills.sh` or discovered in a GitHub repository.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CatalogEntry {
    pub source: String,
    #[serde(rename = "skillId")]
    pub skill_id: String,
    pub name: String,
    #[serde(default)]
    pub installs: u64,
    #[serde(rename = "isOfficial", default)]
    pub is_official: bool,
}

impl CatalogEntry {
    /// Stable identifier for tracking in-flight installs (`source/skill_id`).
    pub fn key(&self) -> String {
        format!("{}/{}", self.source, self.skill_id)
    }

    /// Folder name used on disk: prefers `name` if safe, else `skill_id`.
    pub fn folder_name(&self) -> Option<&str> {
        if valid_skill_name(&self.name) {
            Some(&self.name)
        } else if valid_skill_name(&self.skill_id) {
            Some(&self.skill_id)
        } else {
            None
        }
    }
}

#[derive(Deserialize)]
struct SkillsListResponse {
    #[serde(default)]
    skills: Vec<CatalogEntry>,
}

#[derive(Deserialize)]
struct GitTreeResponse {
    #[serde(default)]
    tree: Vec<GitTreeItem>,
}

#[derive(Deserialize)]
struct GitTreeItem {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into()
    })
}

fn get_bytes(url: &str) -> Result<(u16, Vec<u8>), String> {
    let response = agent()
        .get(url)
        .header("User-Agent", concat!("claudiu/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/json, text/plain, */*")
        .call()
        .map_err(|e| format!("Network request failed: {e}"))?;
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_BYTES_PER_FILE as u64)
        .read_to_vec()
        .map_err(|e| format!("Reading response failed: {e}"))?;
    Ok((status, bytes))
}

/// Top popular skills from `skills.sh`.
pub fn popular() -> Result<Vec<CatalogEntry>, String> {
    let (status, body) = get_bytes(SKILLS_SH_POPULAR)?;
    if status != 200 {
        return Err(format!("skills.sh returned HTTP {status}"));
    }
    parse_skills_response(&body)
}

/// Search `skills.sh` by query, or list skills directly from a GitHub `owner/repo` when the query has that shape.
pub fn search(query: &str) -> Result<Vec<CatalogEntry>, String> {
    let q = query.trim();
    if q.is_empty() {
        return popular();
    }
    if valid_source(q)
        && let Ok(from_repo) = list_repo_skills(q)
        && !from_repo.is_empty()
    {
        return Ok(from_repo);
    }
    if q.len() < 2 {
        return Err("Type at least 2 characters to search skills.sh.".into());
    }
    let url = format!("{SKILLS_SH_SEARCH}?q={}&limit=30", percent_encode(q));
    let (status, body) = get_bytes(&url)?;
    if status != 200 {
        return Err(format!("skills.sh search returned HTTP {status}"));
    }
    parse_skills_response(&body)
}

fn parse_skills_response(body: &[u8]) -> Result<Vec<CatalogEntry>, String> {
    let parsed: SkillsListResponse = serde_json::from_slice(body).map_err(|e| format!("Unexpected response from skills.sh: {e}"))?;
    Ok(parsed.skills)
}

/// Discover every `SKILL.md` inside a GitHub `owner/repo` via the Git Trees API.
pub fn list_repo_skills(source: &str) -> Result<Vec<CatalogEntry>, String> {
    if !valid_source(source) {
        return Err(format!("Invalid GitHub repository “{source}”."));
    }
    let paths = fetch_repo_blobs(source)?;
    Ok(entries_from_tree(source, &paths))
}

fn fetch_repo_blobs(source: &str) -> Result<Vec<String>, String> {
    let url = format!("https://api.github.com/repos/{source}/git/trees/HEAD?recursive=1");
    let (status, body) = get_bytes(&url)?;
    if status != 200 {
        return Err(format!("GitHub returned HTTP {status} for {source}"));
    }
    let tree: GitTreeResponse = serde_json::from_slice(&body).map_err(|e| format!("Unexpected response from GitHub: {e}"))?;
    Ok(tree.tree.into_iter().filter(|item| item.kind == "blob").map(|item| item.path).collect())
}

/// Build catalog entries for every `SKILL.md` found in a repository's file list.
pub fn entries_from_tree(source: &str, blob_paths: &[String]) -> Vec<CatalogEntry> {
    let repo_name = source.split_once('/').map_or(source, |(_, r)| r);
    let mut entries: Vec<CatalogEntry> = blob_paths
        .iter()
        .filter_map(|p| {
            let dir = if p == "SKILL.md" {
                ""
            } else {
                p.strip_suffix("/SKILL.md")?
            };
            let id = dir.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(repo_name);
            valid_skill_name(id).then(|| CatalogEntry {
                source: source.to_string(),
                skill_id: id.to_string(),
                name: id.to_string(),
                installs: 0,
                is_official: false,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries.dedup_by(|a, b| a.name == b.name);
    entries
}

/// Given a repo's blob paths and a `skill_id`, find the directory containing the target `SKILL.md` and every safe
/// relative file path inside that directory.
pub fn locate_skill_files(blob_paths: &[String], skill_id: &str) -> Option<(String, Vec<String>)> {
    let mut candidates: Vec<&str> = blob_paths
        .iter()
        .filter_map(|p| {
            if p == "SKILL.md" {
                Some("")
            } else {
                p.strip_suffix("/SKILL.md")
            }
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Prefer exact folder name match, favoring standard `skills/<id>` / `.agents/skills/<id>` paths and shallower dirs.
    candidates.sort_by_key(|dir| {
        let leaf = dir.rsplit('/').next().unwrap_or(dir);
        let exact = leaf.eq_ignore_ascii_case(skill_id);
        let standard = dir.starts_with("skills/") || dir.starts_with(".agents/skills/") || dir.contains("/skills/");
        (!exact, !standard, dir.matches('/').count(), dir.len())
    });
    let chosen = candidates[0];
    let leaf = chosen.rsplit('/').next().unwrap_or(chosen);
    if !leaf.eq_ignore_ascii_case(skill_id) && candidates.len() > 1 {
        return None;
    }
    let files: Vec<String> = blob_paths
        .iter()
        .filter_map(|p| {
            let rel = if chosen.is_empty() {
                p.as_str()
            } else {
                p.strip_prefix(chosen)?.strip_prefix('/')?
            };
            is_safe_rel_path(rel).then(|| rel.to_string())
        })
        .take(MAX_FILES_PER_SKILL)
        .collect();
    if !files.iter().any(|f| f == "SKILL.md") {
        return None;
    }
    Some((chosen.to_string(), files))
}

fn is_safe_rel_path(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('\\') {
        return false;
    }
    Path::new(rel).components().all(|c| matches!(c, Component::Normal(_)))
}

/// Download `entry` from GitHub into `store_root/<name>/` via a staging directory so failed downloads never leave a
/// broken skill behind. Falls back to probing standard raw GitHub paths if the GitHub tree API is rate-limited.
pub fn install_from_github(store_root: &Path, entry: &CatalogEntry) -> Result<InstalledSkill, String> {
    if !valid_source(&entry.source) {
        return Err(format!("Invalid skill source “{}”.", entry.source));
    }
    let folder_name = entry.folder_name().ok_or_else(|| format!("Invalid skill name “{}”.", entry.name))?;
    fs::create_dir_all(store_root).map_err(|e| format!("Couldn't create skills folder: {e}"))?;
    let staging = store_root.join(format!(".staging-{}", crate::store::new_id()));
    fs::create_dir_all(&staging).map_err(|e| format!("Couldn't create staging folder: {e}"))?;

    let result = download_into(&staging, &entry.source, &entry.skill_id);
    if let Err(err) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }

    let skill_md = fs::read_to_string(staging.join("SKILL.md")).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        format!("Downloaded skill is missing a readable SKILL.md: {e}")
    })?;
    let (_, description) = parse_frontmatter(&skill_md);

    let final_dir = store_root.join(folder_name);
    if final_dir.exists() {
        let _ = fs::remove_dir_all(&final_dir);
    }
    if let Err(e) = fs::rename(&staging, &final_dir) {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("Couldn't place skill into {}: {e}", final_dir.display()));
    }

    Ok(InstalledSkill {
        name: folder_name.to_string(),
        source: entry.source.clone(),
        skill_id: entry.skill_id.clone(),
        description: description.unwrap_or_default(),
        installed_at: crate::store::now(),
    })
}

/// Install every `SKILL.md` package inside a GitHub `owner/repo` using a single Git Trees lookup.
pub fn install_repo_from_github(store_root: &Path, source: &str) -> Result<Vec<InstalledSkill>, String> {
    if !valid_source(source) {
        return Err(format!("Invalid GitHub repository “{source}”."));
    }
    let blobs = fetch_repo_blobs(source)?;
    let entries = entries_from_tree(source, &blobs);
    if entries.is_empty() {
        return Err(format!("No SKILL.md packages found in {source}."));
    }
    fs::create_dir_all(store_root).map_err(|e| format!("Couldn't create skills folder: {e}"))?;
    let mut installed = Vec::new();
    let mut last_err = None;
    for entry in entries {
        let Some(folder_name) = entry.folder_name() else { continue };
        let Some((prefix, files)) = locate_skill_files(&blobs, &entry.skill_id) else { continue };
        let staging = store_root.join(format!(".staging-{}", crate::store::new_id()));
        if let Err(e) = fs::create_dir_all(&staging) {
            last_err = Some(format!("Couldn't create staging folder: {e}"));
            continue;
        }
        let mut ok = true;
        for rel in files {
            let repo_path = if prefix.is_empty() { rel.clone() } else { format!("{prefix}/{rel}") };
            match fetch_raw_file(source, &repo_path).and_then(|bytes| write_staged(&staging, &rel, &bytes)) {
                Ok(()) => {}
                Err(e) => {
                    last_err = Some(e);
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            let _ = fs::remove_dir_all(&staging);
            continue;
        }
        let Ok(skill_md) = fs::read_to_string(staging.join("SKILL.md")) else {
            let _ = fs::remove_dir_all(&staging);
            continue;
        };
        let (_, description) = parse_frontmatter(&skill_md);
        let final_dir = store_root.join(folder_name);
        if final_dir.exists() {
            let _ = fs::remove_dir_all(&final_dir);
        }
        if let Err(e) = fs::rename(&staging, &final_dir) {
            let _ = fs::remove_dir_all(&staging);
            last_err = Some(format!("Couldn't place {}: {e}", final_dir.display()));
            continue;
        }
        installed.push(InstalledSkill {
            name: folder_name.to_string(),
            source: entry.source,
            skill_id: entry.skill_id,
            description: description.unwrap_or_default(),
            installed_at: crate::store::now(),
        });
    }
    if installed.is_empty() {
        return Err(last_err.unwrap_or_else(|| format!("Couldn't install any skills from {source}.")));
    }
    Ok(installed)
}

fn download_into(staging: &Path, source: &str, skill_id: &str) -> Result<(), String> {
    if let Ok(blobs) = fetch_repo_blobs(source)
        && let Some((prefix, files)) = locate_skill_files(&blobs, skill_id)
    {
        for rel in files {
            let repo_path = if prefix.is_empty() { rel.clone() } else { format!("{prefix}/{rel}") };
            let bytes = fetch_raw_file(source, &repo_path)?;
            write_staged(staging, &rel, &bytes)?;
        }
        return Ok(());
    }
    // Fallback when GitHub API is rate-limited or doesn't match: probe standard SKILL.md locations via raw.githubusercontent.com.
    let probes = [
        format!("skills/{skill_id}/SKILL.md"),
        format!(".agents/skills/{skill_id}/SKILL.md"),
        format!("{skill_id}/SKILL.md"),
        "SKILL.md".to_string(),
    ];
    for repo_path in probes {
        if let Ok(bytes) = fetch_raw_file(source, &repo_path) {
            write_staged(staging, "SKILL.md", &bytes)?;
            return Ok(());
        }
    }
    Err(format!("Could not find SKILL.md for “{skill_id}” in {source}."))
}

fn fetch_raw_file(source: &str, repo_path: &str) -> Result<Vec<u8>, String> {
    let encoded_path: String = repo_path.split('/').map(percent_encode).collect::<Vec<_>>().join("/");
    let url = format!("https://raw.githubusercontent.com/{source}/HEAD/{encoded_path}");
    let (status, bytes) = get_bytes(&url)?;
    if status != 200 {
        return Err(format!("Downloading {repo_path} returned HTTP {status}"));
    }
    Ok(bytes)
}

fn write_staged(staging: &Path, rel: &str, bytes: &[u8]) -> Result<(), String> {
    if !is_safe_rel_path(rel) {
        return Err(format!("Refusing unsafe file path “{rel}”."));
    }
    let dst: PathBuf = staging.join(rel);
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Couldn't create folder for {rel}: {e}"))?;
    }
    fs::write(&dst, bytes).map_err(|e| format!("Couldn't write {rel}: {e}"))
}

/// Compact install count for the browser list (`1.6M`, `42.3k`, `310`).
pub fn format_installs(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_standard_nested_and_root_skills_and_keeps_sibling_files() {
        let blobs = vec![
            "README.md".into(),
            "skills/agent-browser/SKILL.md".into(),
            "skills/agent-browser/scripts/run.sh".into(),
            "skills/agent-browser/references/guide.md".into(),
            "skills/other/SKILL.md".into(),
        ];
        let (prefix, files) = locate_skill_files(&blobs, "agent-browser").unwrap();
        assert_eq!(prefix, "skills/agent-browser");
        assert_eq!(files, vec!["SKILL.md", "scripts/run.sh", "references/guide.md"]);

        let plugin_blobs = vec![
            "plugins/m4/skills/implement/SKILL.md".into(),
            "plugins/m4/skills/fix/SKILL.md".into(),
        ];
        let (prefix, files) = locate_skill_files(&plugin_blobs, "implement").unwrap();
        assert_eq!(prefix, "plugins/m4/skills/implement");
        assert_eq!(files, vec!["SKILL.md"]);

        let single = vec!["SKILL.md".into(), "scripts/helper.py".into()];
        let (prefix, files) = locate_skill_files(&single, "anything").unwrap();
        assert_eq!(prefix, "");
        assert_eq!(files, vec!["SKILL.md", "scripts/helper.py"]);
    }

    #[test]
    fn rejects_unsafe_relative_paths() {
        assert!(is_safe_rel_path("SKILL.md"));
        assert!(is_safe_rel_path("scripts/check.sh"));
        assert!(!is_safe_rel_path("../escape"));
        assert!(!is_safe_rel_path("scripts/../../escape"));
        assert!(!is_safe_rel_path("/etc/passwd"));
        assert!(!is_safe_rel_path(""));
    }

    #[test]
    fn parses_skills_sh_json_and_formats_install_counts() {
        let json = br#"{"skills":[{"source":"vercel-labs/agent-browser","skillId":"agent-browser","name":"agent-browser","installs":1634941,"isOfficial":true}]}"#;
        let entries = parse_skills_response(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "vercel-labs/agent-browser");
        assert!(entries[0].is_official);
        assert_eq!(format_installs(entries[0].installs), "1.6M");
        assert_eq!(format_installs(42_300), "42.3k");
        assert_eq!(format_installs(310), "310");
    }

    #[test]
    fn percent_encodes_spaces_and_special_characters() {
        assert_eq!(percent_encode("react native/test"), "react%20native%2Ftest");
    }
}
