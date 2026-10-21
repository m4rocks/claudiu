//! What Claude Code calls over http: the guard hooks (PreToolUse), and later the firewall and the MCP endpoint.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::core::Shared;
use crate::event::SessionId;
use crate::guard::Allowed;

/// The per-process settings that point Claude Code's hooks at this core. Guards fail closed (`onFailure: block`).
pub fn settings(base: &str) -> Value {
    let hook = |event: &str, block: bool| {
        let mut h = json!({"type": "http", "url": format!("{base}/{event}"), "timeout": 10});
        if block {
            h["onFailure"] = json!("block");
        }
        h
    };
    json!({
        "disableAllHooks": false,
        "hooks": {
            "PreToolUse": [{"matcher": "Bash|Write|Edit|MultiEdit|NotebookEdit", "hooks": [hook("pre", true)]}],
            // Fires on every prompt; the first one proves to the canary that hooks run.
            "UserPromptSubmit": [{"hooks": [hook("prompt", false)]}],
        }
    })
}

fn deny(reason: &str) -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}})
}

/// Routes one request: `hook/<token>/<session>/<event>`.
pub fn handle(shared: &Arc<Shared>, path: &str, body: Value) -> (u16, Value) {
    let parts: Vec<&str> = path.split('/').collect();
    let (token, session, event) = match parts.as_slice() {
        ["hook", token, session, event] => (*token, *session, *event),
        ["mcp", token, session] => (*token, *session, "mcp"),
        _ => return (404, Value::Null),
    };
    if token != shared.token {
        return (403, Value::Null);
    }
    let session = SessionId(session.to_string());
    let Some(info) = shared.session(&session) else { return (404, Value::Null) };
    match event {
        "pre" => {
            info.hooks_fired();
            (200, pre(shared, &session, &info.cwd, &body))
        }
        "prompt" => {
            info.hooks_fired();
            let notes = info.take_notes();
            if notes.is_empty() {
                return (200, json!({}));
            }
            (200, json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": notes.join(" ")}}))
        }
        "mcp" => match crate::mcp::handle(shared, &session, &info, &body) {
            Some(reply) => (200, reply),
            None => (202, Value::Null),
        },
        _ => (404, Value::Null),
    }
}

fn pre(shared: &Shared, session: &SessionId, cwd: &Path, body: &Value) -> Value {
    let input = &body["tool_input"];
    let grants = shared.policy.grants(session, cwd);
    let allowed = Allowed { cwd, toolchain_dirs: &shared.guard.toolchain_dirs, grants: &grants };
    match body["tool_name"].as_str().unwrap_or("") {
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let target = input["file_path"].as_str().or_else(|| input["notebook_path"].as_str());
            match target {
                Some(t) if !allowed.permits(Path::new(t)) => deny(&allowed.refusal(&PathBuf::from(t))),
                _ => json!({}),
            }
        }
        "Bash" => match input["command"].as_str().map(|c| crate::shell::check(c, &allowed)) {
            Some(crate::shell::Verdict::Refuse(why)) => deny(&why),
            Some(crate::shell::Verdict::Outside(path)) => deny(&allowed.refusal(&path)),
            _ => json!({}),
        },
        _ => json!({}),
    }
}
