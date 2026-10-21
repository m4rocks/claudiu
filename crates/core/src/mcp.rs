//! The `claudiu` MCP server, served over http by the core itself (no helper process).
//!
//! Tool descriptions are short on purpose: every token here is paid on every request of every session.

use std::path::Path;

use serde_json::{Value, json};

use crate::core::{Shared, SessionInfo};
use crate::event::{SessionId, kind};
use crate::guard::{Allowed, resolve};

fn tools() -> Value {
    json!([
        {
            "name": "request_access",
            "description": "Ask the user for write access to a path outside the project. Returns at once; keep working.",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}, "reason": {"type": "string"}},
                "required": ["path", "reason"]
            }
        }
    ])
}

fn text(t: &str) -> Value {
    json!({"content": [{"type": "text", "text": t}]})
}

/// Handles one JSON-RPC request; `None` is a notification that gets an empty 202.
pub fn handle(shared: &Shared, session: &SessionId, info: &SessionInfo, body: &Value) -> Option<Value> {
    let id = body.get("id").cloned()?;
    let result = match body["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": body["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "claudiu", "version": env!("CARGO_PKG_VERSION")}
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call(shared, session, info, &body["params"]),
        other => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("{other} is not supported")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn call(shared: &Shared, session: &SessionId, info: &SessionInfo, params: &Value) -> Value {
    let args = &params["arguments"];
    match params["name"].as_str().unwrap_or("") {
        "request_access" => {
            let (Some(path), reason) = (args["path"].as_str(), args["reason"].as_str().unwrap_or("")) else {
                return json!({"isError": true, "content": [{"type": "text", "text": "request_access needs a path and a reason."}]});
            };
            let target = resolve(Path::new(path), &info.cwd);
            let grants = shared.policy.grants(session, &info.cwd);
            let allowed = Allowed { cwd: &info.cwd, toolchain_dirs: &shared.guard.toolchain_dirs, grants: &grants };
            if allowed.permits(&target) {
                return text("Already allowed; no request needed.");
            }
            let request = info.add_request(target.clone(), reason.to_string());
            let _ = shared.rec.record(session, kind::ACCESS_REQUESTED, json!({"request_id": request, "path": target.to_string_lossy(), "reason": reason}));
            text("Pending: the user has been asked. Keep working on other things; you will be told the answer.")
        }
        other => json!({"isError": true, "content": [{"type": "text", "text": format!("Unknown tool {other}.")}]}),
    }
}
