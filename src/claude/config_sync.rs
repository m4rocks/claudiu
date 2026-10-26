//! One Claude Code configuration across the main account and every alt account, kept equal in both directions.
//!
//! Each account has its own config folder, so the same setting exists once per account. A setting changed in any
//! account (or outside Claudiu) is copied to the others. Nothing is linked: Claude Code saves these files by replacing
//! them, which breaks a link and lets the copies drift.
//!
//! "Changed" means different from the value every account agreed on last time (`State`), not newer: Claude Code
//! rewrites its files all the time, so the newest file is not necessarily the one somebody edited. When several
//! accounts changed, the newest change wins. A removed file is not copied as a removal.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Files inside each account's config folder that every account shares.
const FILES: &[&str] = &["settings.json", "CLAUDE.md", "keybindings.json"];
/// Top-level keys of an account's `.claude.json` that every account shares: the user's MCP servers.
const SHARED_KEYS: &[&str] = &["mcpServers"];
/// One project's trust in `.claude.json`: once any account trusts a folder, every account does.
const TRUST_KEY: &str = "hasTrustDialogAccepted";

/// Runs one at a time: a sync at launch and the background one must not interleave.
static RUNNING: Mutex<()> = Mutex::new(());

/// Where one account keeps its configuration.
#[derive(Clone, Debug)]
pub struct Account {
    /// Its config folder (`~/.claude` for the main account).
    pub dir: PathBuf,
    /// Its `.claude.json`: login, MCP servers and project trust. Claude Code rewrites it whole and often, so it is
    /// only written while no session uses it (`busy`).
    pub claude_json: PathBuf,
    pub busy: bool,
}

/// What every account held the last time they agreed, as fingerprints, keyed by what was compared.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    agreed: BTreeMap<String, u64>,
    /// `key|account` for every account that held that key when everyone last agreed.
    #[serde(default)]
    had: BTreeSet<String>,
}

impl State {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        crate::datadir::write_atomic(path, &serde_json::to_string(self).map_err(io::Error::other)?)
    }
}

pub fn state_path() -> PathBuf {
    crate::datadir::data_dir().join("config-sync.json")
}

/// The main account, and the alt folders given: where each keeps its files. The main `.claude.json` is `~/.claude.json`
/// unless `CLAUDE_CONFIG_DIR` moves it into the folder.
pub fn main_account(busy: bool) -> Option<Account> {
    let dir = super::claude_home()?;
    let claude_json = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(_) => dir.join(".claude.json"),
        None => super::home_dir()?.join(".claude.json"),
    };
    Some(Account { dir, claude_json, busy })
}

pub fn alt_account(dir: &Path, busy: bool) -> Account {
    Account { dir: dir.to_path_buf(), claude_json: dir.join(".claude.json"), busy }
}

/// Bring every account to the same configuration. Does what it can and returns the first error.
pub fn sync(accounts: &[Account], state_file: &Path) -> io::Result<()> {
    let _one_at_a_time = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = State::load(state_file);
    let mut result = Ok(());
    for name in FILES {
        let paths: Vec<PathBuf> = accounts.iter().map(|a| a.dir.join(name)).collect();
        keep(&mut result, sync_files(&paths, &format!("file:{name}"), &mut state));
    }
    keep(&mut result, sync_claude_json(accounts, &mut state));
    keep(&mut result, state.save(state_file));
    result
}

fn keep(result: &mut io::Result<()>, next: io::Result<()>) {
    if result.is_ok() {
        *result = next;
    }
}

/// FNV-1a: stable between Rust versions, unlike a std hasher, since fingerprints are saved.
fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

/// What one account holds for a thing being compared.
struct Held<T> {
    value: T,
    print: u64,
    modified: SystemTime,
}

/// The account whose value the others should take, or `None` when nobody holds one. Changed since `agreed` wins over
/// unchanged, then the newest, then the earliest account (the main one).
fn pick<T>(held: &[Option<Held<T>>], agreed: Option<u64>) -> Option<usize> {
    let present = || held.iter().enumerate().filter_map(|(i, h)| h.as_ref().map(|h| (i, h)));
    let changed: Vec<_> = present().filter(|(_, h)| Some(h.print) != agreed).collect();
    let candidates = if changed.is_empty() { present().collect() } else { changed };
    candidates.into_iter().min_by(|(ia, a), (ib, b)| b.modified.cmp(&a.modified).then(ia.cmp(ib))).map(|(i, _)| i)
}

/// Copy the winning file over the others. A file is written beside its target and renamed over it, which replaces the
/// directory entry and never writes through a link left by an earlier Claudiu.
fn sync_files(paths: &[PathBuf], key: &str, state: &mut State) -> io::Result<()> {
    let held: Vec<Option<Held<Vec<u8>>>> = paths
        .iter()
        .map(|p| {
            let value = fs::read(p).ok()?;
            let modified = fs::metadata(p).and_then(|m| m.modified()).ok()?;
            Some(Held { print: fingerprint(&value), value, modified })
        })
        .collect();
    let Some(winner) = pick(&held, state.agreed.get(key).copied()) else { return Ok(()) };
    let source = held[winner].as_ref().expect("the winner holds a value");
    let mut result = Ok(());
    for (path, have) in paths.iter().zip(&held) {
        if have.as_ref().is_some_and(|h| h.print == source.print) {
            continue;
        }
        let tmp = path.with_extension("claudiu-tmp");
        let written = fs::write(&tmp, &source.value).and_then(|()| fs::rename(&tmp, path));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        keep(&mut result, written);
    }
    // Only when everyone took it: an account that could not would otherwise look "changed" next time and push back.
    if result.is_ok() {
        state.agreed.insert(key.into(), source.print);
    }
    result
}

/// A JSON value written the same way whatever the order of its keys, so equal settings have equal fingerprints.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map.iter().collect();
            let items: Vec<String> = sorted.into_iter().map(|(k, v)| format!("{}:{}", Value::String(k.clone()), canonical(v))).collect();
            format!("{{{}}}", items.join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

/// Shared keys and project trust across the accounts' `.claude.json` files. A file that cannot be read or parsed is
/// left out, and one that is `busy` is read but not written. Nothing is recorded as agreed until every account took it.
fn sync_claude_json(accounts: &[Account], state: &mut State) -> io::Result<()> {
    let mut files: Vec<Option<(Map<String, Value>, SystemTime)>> = accounts
        .iter()
        .map(|a| {
            let Value::Object(map) = serde_json::from_str(&fs::read_to_string(&a.claude_json).ok()?).ok()? else { return None };
            Some((map, fs::metadata(&a.claude_json).and_then(|m| m.modified()).ok()?))
        })
        .collect();
    let mut dirty = vec![false; accounts.len()];
    let mut incomplete = false;
    let mut agreed_now: Vec<(String, u64, Vec<usize>)> = vec![];

    for key in SHARED_KEYS {
        let state_key = format!("json:{key}");
        let held: Vec<Option<Held<Option<Value>>>> = files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (map, modified) = f.as_ref()?;
                let value = map.get(*key).cloned();
                // A missing key is a removal only where the key was seen at the last agreement; elsewhere (a new
                // account) it is a gap to fill, so a fresh account cannot wipe the others' servers.
                if value.is_none() && !state.had.contains(&had_key(&state_key, &accounts[i])) {
                    return None;
                }
                let print = fingerprint(value.as_ref().map(canonical).unwrap_or_default().as_bytes());
                Some(Held { value, print, modified: *modified })
            })
            .collect();
        let Some(winner) = pick(&held, state.agreed.get(&state_key).copied()) else { continue };
        let source = held[winner].as_ref().expect("the winner holds a value");
        let (print, value) = (source.print, source.value.clone());
        for (i, file) in files.iter_mut().enumerate() {
            let Some((map, _)) = file else { continue };
            if map.get(*key) == value.as_ref() {
                continue;
            }
            if accounts[i].busy {
                incomplete = true;
                continue;
            }
            match &value {
                Some(value) => map.insert((*key).into(), value.clone()),
                None => map.remove(*key),
            };
            dirty[i] = true;
        }
        let holders = if value.is_some() { (0..files.len()).filter(|i| files[*i].is_some()).collect() } else { vec![] };
        agreed_now.push((state_key, print, holders));
    }

    // Trust only grows: a folder any account trusts is trusted everywhere.
    let trusted: HashSet<String> = files
        .iter()
        .flatten()
        .filter_map(|(map, _)| map.get("projects")?.as_object())
        .flat_map(|projects| projects.iter().filter(|(_, p)| p.get(TRUST_KEY) == Some(&Value::Bool(true))).map(|(path, _)| path.clone()))
        .collect();
    for (i, file) in files.iter_mut().enumerate() {
        let Some((map, _)) = file else { continue };
        let untrusting: Vec<&String> = trusted
            .iter()
            .filter(|path| map.get("projects").and_then(|p| p.get(path.as_str())).and_then(|p| p.get(TRUST_KEY)) != Some(&Value::Bool(true)))
            .collect();
        if untrusting.is_empty() {
            continue;
        }
        if accounts[i].busy {
            incomplete = true;
            continue;
        }
        let Some(projects) = map.entry("projects").or_insert_with(|| Value::Object(Map::new())).as_object_mut() else { continue };
        for path in untrusting {
            if let Some(project) = projects.entry(path.clone()).or_insert_with(|| Value::Object(Map::new())).as_object_mut() {
                project.insert(TRUST_KEY.into(), Value::Bool(true));
                dirty[i] = true;
            }
        }
    }

    let mut result = Ok(());
    for (i, file) in files.iter().enumerate() {
        let (true, Some((map, _))) = (dirty[i], file) else { continue };
        let text = serde_json::to_string_pretty(map).map_err(io::Error::other);
        let written = text.and_then(|text| crate::datadir::write_atomic(&accounts[i].claude_json, &text));
        incomplete |= written.is_err();
        keep(&mut result, written);
    }
    if !incomplete {
        for (state_key, print, holders) in agreed_now {
            state.agreed.insert(state_key.clone(), print);
            state.had.retain(|h| !h.starts_with(&format!("{state_key}|")));
            state.had.extend(holders.into_iter().map(|i| had_key(&state_key, &accounts[i])));
        }
    }
    result
}

fn had_key(state_key: &str, account: &Account) -> String {
    format!("{state_key}|{}", account.dir.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn holding(text: &str, secs: u64) -> Option<Held<Vec<u8>>> {
        Some(Held { value: text.as_bytes().to_vec(), print: fingerprint(text.as_bytes()), modified: at(secs) })
    }

    #[test]
    fn the_account_that_changed_wins_even_when_another_file_is_newer() {
        let agreed = fingerprint(b"v1");
        // main was only touched later (Claude Code rewrote it); the alt holds the edit.
        let held = [holding("v1", 500), holding("edited in alt", 100), holding("v1", 300)];
        assert_eq!(pick(&held, Some(agreed)), Some(1));
    }

    #[test]
    fn of_several_changes_the_newest_wins_and_a_tie_goes_to_the_earlier_account() {
        let agreed = fingerprint(b"v1");
        assert_eq!(pick(&[holding("a", 100), holding("b", 200)], Some(agreed)), Some(1));
        assert_eq!(pick(&[holding("a", 100), holding("b", 100)], Some(agreed)), Some(0));
    }

    #[test]
    fn without_a_record_the_newest_file_wins_and_a_missing_one_is_filled() {
        assert_eq!(pick(&[holding("old", 100), holding("new", 200)], None), Some(1));
        assert_eq!(pick(&[None, holding("only", 100)], None), Some(1));
        assert_eq!(pick::<Vec<u8>>(&[None, None], None), None);
    }

    struct Setup {
        _tmp: tempfile::TempDir,
        accounts: Vec<Account>,
        state: PathBuf,
    }

    fn setup(count: usize) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let accounts = (0..count)
            .map(|i| {
                let dir = tmp.path().join(format!("acct{i}"));
                fs::create_dir_all(&dir).unwrap();
                Account { claude_json: dir.join(".claude.json"), dir, busy: false }
            })
            .collect();
        Setup { state: tmp.path().join("state.json"), accounts, _tmp: tmp }
    }

    fn write_at(path: &Path, text: &str, secs: u64) {
        fs::write(path, text).unwrap();
        fs::File::options().write(true).open(path).unwrap().set_modified(at(secs)).unwrap();
    }

    fn read(account: &Account, name: &str) -> String {
        fs::read_to_string(account.dir.join(name)).unwrap()
    }

    #[test]
    fn an_edit_in_any_account_reaches_all_the_others_and_nothing_bounces_back() {
        let s = setup(3);
        for a in &s.accounts {
            write_at(&a.dir.join("settings.json"), "{\"model\":\"opus\"}", 1000);
        }
        sync(&s.accounts, &s.state).unwrap();

        // The second alt edits its copy; main is touched later without a change.
        write_at(&s.accounts[2].dir.join("settings.json"), "{\"model\":\"sonnet\"}", 2000);
        sync(&s.accounts, &s.state).unwrap();
        assert!(s.accounts.iter().all(|a| read(a, "settings.json") == "{\"model\":\"sonnet\"}"));

        // Then main edits: it spreads the other way.
        write_at(&s.accounts[0].dir.join("settings.json"), "{\"model\":\"haiku\"}", 3000);
        sync(&s.accounts, &s.state).unwrap();
        assert!(s.accounts.iter().all(|a| read(a, "settings.json") == "{\"model\":\"haiku\"}"));
    }


    #[test]
    fn a_missing_file_is_filled_but_a_removed_one_is_not_copied_as_a_removal() {
        let s = setup(2);
        write_at(&s.accounts[0].dir.join("CLAUDE.md"), "rules", 1000);
        sync(&s.accounts, &s.state).unwrap();
        assert_eq!(read(&s.accounts[1], "CLAUDE.md"), "rules");
        fs::remove_file(s.accounts[0].dir.join("CLAUDE.md")).unwrap();
        sync(&s.accounts, &s.state).unwrap();
        assert_eq!(read(&s.accounts[1], "CLAUDE.md"), "rules");
        assert_eq!(read(&s.accounts[0], "CLAUDE.md"), "rules", "it comes back from the account that still has it");
    }

    #[test]
    fn mcp_servers_and_trust_are_shared_and_the_rest_of_claude_json_is_left_alone() {
        let s = setup(2);
        write_at(&s.accounts[0].claude_json, r#"{"oauthAccount":{"email":"main"},"mcpServers":{"a":{"command":"x"}},"projects":{"C:/p":{"hasTrustDialogAccepted":true,"allowedTools":["t"]}}}"#, 1000);
        write_at(&s.accounts[1].claude_json, r#"{"oauthAccount":{"email":"alt"},"projects":{"C:/q":{"hasTrustDialogAccepted":true}}}"#, 1000);
        sync(&s.accounts, &s.state).unwrap();
        let alt: Value = serde_json::from_str(&fs::read_to_string(&s.accounts[1].claude_json).unwrap()).unwrap();
        let main: Value = serde_json::from_str(&fs::read_to_string(&s.accounts[0].claude_json).unwrap()).unwrap();
        assert_eq!(alt["mcpServers"]["a"]["command"], "x");
        assert_eq!(alt["oauthAccount"]["email"], "alt", "the login is the account's own");
        assert_eq!(alt["projects"]["C:/p"][TRUST_KEY], true);
        assert_eq!(main["projects"]["C:/q"][TRUST_KEY], true);
        assert!(alt["projects"]["C:/p"].get("allowedTools").is_none(), "only trust is shared, not the project's other state");

        // Removing the servers in the alt removes them everywhere.
        let mut alt: Value = serde_json::from_str(&fs::read_to_string(&s.accounts[1].claude_json).unwrap()).unwrap();
        alt.as_object_mut().unwrap().remove("mcpServers");
        write_at(&s.accounts[1].claude_json, &alt.to_string(), 2000);
        sync(&s.accounts, &s.state).unwrap();
        let main: Value = serde_json::from_str(&fs::read_to_string(&s.accounts[0].claude_json).unwrap()).unwrap();
        assert!(main.get("mcpServers").is_none(), "the removal reached main");
    }

    #[test]
    fn a_busy_accounts_claude_json_is_not_written_and_catches_up_when_idle() {
        let mut s = setup(2);
        write_at(&s.accounts[0].claude_json, r#"{"mcpServers":{"a":{}}}"#, 1000);
        write_at(&s.accounts[1].claude_json, r#"{"login":1}"#, 1000);
        s.accounts[1].busy = true;
        let before = fs::read_to_string(&s.accounts[1].claude_json).unwrap();
        sync(&s.accounts, &s.state).unwrap();
        assert_eq!(fs::read_to_string(&s.accounts[1].claude_json).unwrap(), before);
        s.accounts[1].busy = false;
        sync(&s.accounts, &s.state).unwrap();
        let alt: Value = serde_json::from_str(&fs::read_to_string(&s.accounts[1].claude_json).unwrap()).unwrap();
        assert!(alt["mcpServers"].get("a").is_some());
    }
}
