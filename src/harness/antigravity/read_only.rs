//! Read-only Antigravity subagents. Antigravity has no read-only mode: in its default mode it writes files in the
//! workspace without asking, and `agy acp` takes no per-session settings. What it does honour is a PreToolUse hook
//! that answers `deny`, read from the `config/hooks.json` of its Gemini folder. So a read-only run starts agy with
//! `--gemini-dir` pointing at a folder in m4code's data, whose hook runs `m4code --acp --read-only-hook`, which lets
//! through only the tools that read or search. Sign-in lives in the system keyring, so the run stays signed in; the
//! user's own Antigravity rules, skills and MCP servers, kept in their own Gemini folder, don't load.
//!
//! agy runs a hook that fails as if it had allowed the tool, so the hook script answers `deny` itself when m4code
//! can't be started.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const HOOK_FLAG: &str = "--read-only-hook";

/// The tools that only read or search, by the names agy gives them. Anything else, the shell and every edit among
/// them, is denied, so a tool agy adds later is refused until it is listed here.
const READ_TOOLS: &[&str] = &[
    "read_file",
    "view_file",
    "view_file_outline",
    "view_code_item",
    "view_content_chunk",
    "list_dir",
    "list_directory",
    "grep_search",
    "find_by_name",
    "code_search",
    "codebase_search",
    "find_all_references",
    "search_web",
    "web_search",
    "read_url",
    "read_url_content",
    "mcp__m4code__workspace_context",
    "mcp_m4code_workspace_context",
    "m4code_workspace_context",
];

/// ASCII only: the script's fallback echoes it through cmd, whose code page could garble anything else into JSON agy
/// can't read, and agy runs a tool whose hook gave no answer.
const DENIED: &str = "m4code runs this Antigravity subagent read-only: it can read and search, but not change files or run commands. Turn on Allow edits in Settings > Custom subagents to let it.";

/// The Gemini folder read-only runs use, made ready with the hook, which starts the m4code running now.
pub fn prepare(data_dir: &Path, m4code: &Path) -> std::io::Result<PathBuf> {
    prepare_with_hook(data_dir.join("antigravity-read-only"), &hook_script(m4code))
}

pub fn prepare_text_generation(data_dir: &Path) -> std::io::Result<PathBuf> {
    let denied = json!({"decision": "deny", "reason": "Tools are disabled for text generation."}).to_string();
    let script = if cfg!(windows) {
        format!("@echo off\r\necho {denied}\r\n")
    } else {
        format!("#!/bin/sh\nprintf '%s\\n' '{denied}'\n")
    };
    prepare_with_hook(data_dir.join("antigravity-text-generation"), &script)
}

fn prepare_with_hook(gemini: PathBuf, body: &str) -> std::io::Result<PathBuf> {
    let config = gemini.join("config");
    std::fs::create_dir_all(&config)?;
    let script = config.join(if cfg!(windows) { "read-only-hook.cmd" } else { "read-only-hook.sh" });
    publish(&script, body)?;
    publish(&config.join("hooks.json"), &hooks_json(&script).to_string())?;
    Ok(gemini)
}

/// Writes `body` to `path` unless it is there already. Another read-only subagent may be running the hook at this
/// moment, and a hook file it reads half-written gives agy no answer, which agy takes as allowed. So the file is
/// written beside it and moved over it whole.
fn publish(path: &Path, body: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == body) {
        return Ok(());
    }
    let staged = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&staged, body)?;
    #[cfg(unix)]
    std::fs::set_permissions(&staged, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    std::fs::rename(&staged, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// agy starts the hook from its own working folder, not the hook's, so the script is named by its full path, quoted
/// for a path with spaces.
fn hooks_json(script: &Path) -> Value {
    let command = format!("\"{}\"", script.display());
    json!({ "m4code-read-only": { "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": command, "timeout": 10 }] }] } })
}

/// Runs m4code's check, and denies when m4code can't be started.
fn hook_script(m4code: &Path) -> String {
    let denied = json!({ "decision": "deny", "reason": DENIED }).to_string();
    if cfg!(windows) {
        // cmd's echo prints quotes as they are; `^` keeps its own meaning off the characters it treats specially.
        let echoed: String = denied.chars().flat_map(|c| if matches!(c, '&' | '|' | '<' | '>' | '^' | '(' | ')') { vec!['^', c] } else { vec![c] }).collect();
        format!("@echo off\r\n\"{}\" --acp {HOOK_FLAG} || echo {echoed}\r\n", m4code.display())
    } else {
        let quoted = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        format!("#!/bin/sh\n{} --acp {HOOK_FLAG} || echo {}\n", quoted(&m4code.to_string_lossy()), quoted(&denied))
    }
}

/// What the hook answers for a PreToolUse payload: allow a reading tool, deny the rest and anything unreadable.
pub fn decision(payload: &str) -> Value {
    let tool = serde_json::from_str::<Value>(payload).ok().and_then(|p| p["toolCall"]["name"].as_str().map(str::to_owned));
    match tool {
        Some(name) if READ_TOOLS.contains(&name.as_str()) => json!({ "decision": "allow" }),
        _ => json!({ "decision": "deny", "reason": DENIED }),
    }
}

/// `m4code --acp --read-only-hook`: one PreToolUse payload on stdin, the decision on stdout.
pub fn run_hook() -> i32 {
    let mut payload = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut payload);
    println!("{}", decision(&payload));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(tool: &str) -> String {
        json!({ "hookEventName": "PreToolUse", "toolCall": { "id": "c1", "name": tool, "args": {} }, "cwd": "/work" }).to_string()
    }

    #[test]
    fn only_reading_tools_are_allowed() {
        for tool in ["read_file", "grep_search", "list_dir"] {
            assert_eq!(decision(&payload(tool))["decision"], "allow", "{tool}");
        }
        for tool in ["write_file", "run_command", "replace_file_content", "delete_file", "browser_subagent", "some_future_tool"] {
            assert_eq!(decision(&payload(tool))["decision"], "deny", "{tool}");
        }
        assert_eq!(decision("not json")["decision"], "deny");
        assert_eq!(decision(r#"{"toolCall":{}}"#)["decision"], "deny");
    }

    #[test]
    fn the_folder_holds_a_hook_that_runs_m4code_by_its_full_path() {
        let data = tempfile::tempdir().unwrap();
        let m4code = Path::new(if cfg!(windows) { r"C:\Program Files\m4code\m4code.exe" } else { "/opt/it's m4code/m4code" });
        let gemini = prepare(data.path(), m4code).unwrap();
        assert_eq!(gemini, data.path().join("antigravity-read-only"));
        let hooks: Value = serde_json::from_str(&std::fs::read_to_string(gemini.join("config/hooks.json")).unwrap()).unwrap();
        let group = &hooks["m4code-read-only"]["PreToolUse"][0];
        assert_eq!(group["matcher"], "*", "every tool goes through the check");
        let command = group["hooks"][0]["command"].as_str().unwrap();
        let script = command.trim_matches('"');
        assert!(Path::new(script).is_absolute() && command.starts_with('"'), "{command}");
        let body = std::fs::read_to_string(script).unwrap();
        assert!(body.contains(&format!("--acp {HOOK_FLAG} || echo ")), "the script denies when m4code fails: {body}");
        if cfg!(windows) {
            assert!(body.contains(r#""C:\Program Files\m4code\m4code.exe" --acp"#), "{body}");
        } else {
            assert!(body.contains(r"'/opt/it'\''s m4code/m4code' --acp"), "{body}");
        }
        // A second run finds the files as they should be and leaves them alone, since a running hook may read them.
        let written = std::fs::metadata(script).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(prepare(data.path(), m4code).unwrap(), gemini);
        assert_eq!(std::fs::metadata(script).unwrap().modified().unwrap(), written);
        // A m4code that moved writes a new script, whole.
        prepare(data.path(), &data.path().join("moved")).unwrap();
        assert!(std::fs::read_to_string(script).unwrap().contains("moved"));
        let left: Vec<_> = std::fs::read_dir(gemini.join("config")).unwrap().flatten().filter(|e| e.path().to_string_lossy().ends_with(".tmp")).collect();
        assert!(left.is_empty(), "no staged file is left behind");
    }

    #[test]
    #[cfg(windows)]
    fn the_script_denies_when_m4code_is_missing() {
        let data = tempfile::tempdir().unwrap();
        let gemini = prepare(data.path(), &data.path().join("gone.exe")).unwrap();
        let mut command = std::process::Command::new("cmd");
        command.arg("/c").arg(gemini.join("config").join("read-only-hook.cmd"));
        let out = command.stdin(std::process::Stdio::null()).output().unwrap();
        let answer: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        assert_eq!(answer["decision"], "deny");
    }

    #[test]
    #[cfg(unix)]
    fn the_script_denies_when_m4code_is_missing() {
        let data = tempfile::tempdir().unwrap();
        let gemini = prepare(data.path(), &data.path().join("gone")).unwrap();
        let out = std::process::Command::new(gemini.join("config").join("read-only-hook.sh")).stdin(std::process::Stdio::null()).output().unwrap();
        let answer: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        assert_eq!(answer["decision"], "deny");
    }
}
