//! m4code's built-in MCP server (`m4code --mcp-title`), wired into every harness session over stdio.
//!
//! Provides shared tools for GitHub attachment, session linking, model switching, subagents, and questions.
//! Session-specific instructions describe the workspace and the app's inline UI.

pub mod bridge;
pub mod session;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use serde::{Deserialize, Serialize};

use crate::store::Attached;

pub const FLAG: &str = "--mcp-title";
/// m4code's own id of the session. Unlike Claude's session id it survives `/clear`.
pub const RECORD_ENV: &str = "M4CODE_RECORD";
/// Comma-separated names of what the server leaves out of this session (see `tools_off`). Missing means nothing is off.
pub const TOOLS_OFF_ENV: &str = "M4CODE_TOOLS_OFF";

/// A group of m4code's tools the user can switch off (Settings → m4code tools).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Visual,
    Github,
    Sessions,
    Model,
}

impl Feature {
    /// In the order the page lists them.
    pub const ALL: [Feature; 4] = [Self::Visual, Self::Github, Self::Sessions, Self::Model];

    pub fn label(self) -> &'static str {
        match self {
            Self::Visual => "Inline UI",
            Self::Github => "GitHub sidebar",
            Self::Sessions => "Other sessions",
            Self::Model => "Model switch",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Visual => "Render interactive cards, charts, and diagrams directly in chat",
            Self::Github => "Attach GitHub issues and pull requests to the right sidebar",
            Self::Sessions => "Link and coordinate with other running m4code sessions",
            Self::Model => "Let the agent switch its own model or effort mid-conversation",
        }
    }

    /// The name in `M4CODE_TOOLS_OFF`.
    fn name(self) -> &'static str {
        match self {
            Self::Visual => "visual",
            Self::Github => "github",
            Self::Sessions => "sessions",
            Self::Model => "model",
        }
    }
}

/// `features` in the page's order, without repeats: how they are stored, so equal sets compare equal.
pub fn normalized_features(features: &[Feature]) -> Vec<Feature> {
    Feature::ALL.into_iter().filter(|f| features.contains(f)).collect()
}

/// What the server leaves out of a session: the features switched off plus any adapter-provided tool names to hide.
pub fn tools_off(features: &[Feature], extra_off: &[&'static str]) -> Vec<&'static str> {
    let mut off: Vec<&'static str> = normalized_features(features).into_iter().map(Feature::name).collect();
    for &name in extra_off {
        if !off.contains(&name) {
            off.push(name);
        }
    }
    off
}

/// The feature a tool belongs to.
fn feature_of(tool: &str) -> Option<&'static str> {
    match tool {
        "attach_github" | "detach_github" => Some("github"),
        "link_session" | "unlink_session" => Some("sessions"),
        "set_model" | "list_models" => Some("model"),
        "ask_user" => Some("ask_user"),
        _ if BRIDGE_TOOLS.contains(&tool) || SUBAGENT_TOOLS.contains(&tool) => Some("sessions"),
        _ => None,
    }
}

fn is_off(off: &[&str], tool: &str) -> bool {
    feature_of(tool).is_some_and(|f| off.contains(&f))
}

/// Tools the model uses to tell m4code something about itself; none of them reads or starts another session.
const OWN_TOOLS: [&str; 4] = ["attach_github", "detach_github", "link_session", "unlink_session"];

/// Tools that change this session's model; m4code answers them (`app/cross.rs`), whatever the harness.
const MODEL_TOOLS: [&str; 2] = ["list_models", "set_model"];

/// Tools that reach other sessions (`mcp/bridge.rs`).
pub(crate) const SUBAGENT_TOOLS: [&str; 5] = ["spawn_subagent", "list_subagents", "send_subagent_message", "wait_subagent", "stop_subagent"];

const BRIDGE_TOOLS: [&str; 6] = ["list_code_sessions", "read_code_session", "search_sessions", "search_projects", "start_code_session", "switch_worktree"];

/// Interactive UI tools served by m4code across every harness (including ones whose native question tool isn't bridged over ACP).
const INTERACTIVE_TOOLS: [&str; 1] = ["ask_user"];

/// `mcp__<server>__<tool>` names allowed per process so Claude Code adds no prompt of its own: m4code's own tools only
/// change what m4code shows, and the cross-session tools are limited to what the user asks of the model.
pub fn allowed_tools(off: &[&str]) -> Vec<String> {
    OWN_TOOLS.iter().copied().chain(MODEL_TOOLS).chain(INTERACTIVE_TOOLS).chain(BRIDGE_TOOLS).chain(SUBAGENT_TOOLS).filter(|t| !is_off(off, t)).map(|t| format!("mcp__m4code__{t}")).collect()
}

/// Whether a tool call's title is one of m4code's own tools and nothing else, as a harness names it
/// (`m4code: list_models`, `m4code_list_models`, `mcp__m4code__list_models`). Their permission prompts are answered
/// yes: the instructions already limit them to what the user asked for. The whole title must match, because a shell
/// command or a file path can contain the same words.
pub fn is_own_tool(title: &str) -> bool {
    let title = title.trim();
    OWN_TOOLS.iter().chain(&MODEL_TOOLS).chain(&INTERACTIVE_TOOLS).chain(&BRIDGE_TOOLS).chain(&SUBAGENT_TOOLS).chain(&["workspace_context"]).any(|tool| {
        ["mcp__m4code__", "m4code: ", "m4code_", "m4code.", "m4code/", "m4code__"].iter().any(|prefix| title.strip_prefix(prefix) == Some(tool))
    })
}

const BRIDGE_INSTRUCTIONS: &str = " Sessions are user-owned conversations. Only create a session when the user wants a new session. \
m4code has tools that reach the user's sessions: list_code_sessions, read_code_session, search_sessions, search_projects and start_code_session. \
Use the read tools when the user asks you to inspect existing sessions or projects; search_sessions may also be used unasked to find a session to link. \
start_code_session creates a conversation for the user, not a worker for the model. It takes a project, a model and an effort; \
name a model or effort only when the user did. Never ask the user to confirm a session they already requested. \
switch_worktree moves this session into another git worktree of its project; call it only when the user asks for a worktree, and \
end your turn after it.";

const SUBAGENT_INSTRUCTIONS: &str = " Subagents are model-owned workers for delegated work within the current session. \
Use subagents when you need another agent to help with your task; their reports return here. Sessions are for the user, subagents are for the model. \
Subagents run through m4code on enabled adapters. Use spawn_subagent to delegate a task, list_subagents to inspect children, \
send_subagent_message for a follow-up and stop_subagent to cancel. spawn_subagent returns at once; when a child's turn \
ends, m4code sends you its report as a message starting with [m4code subagent report], so don't poll. Call \
wait_subagent only when you must have the report before you can go on in this turn. \
These tools use the subagent and edit permissions saved in Settings. Follow the user's and project's delegation rules.";

const LINK_INSTRUCTIONS: &str = " m4code shows related sessions together. When this work continues or builds on another \
m4code session (the user says so, or you recognise it), find it with search_sessions and call link_session once per pair; \
call unlink_session when a link no longer fits. Both are silent.";

/// What teaches the model to emit native inline ` ```ui ` generative UI blocks directly in chat. It comes right after
/// the opening, because some harnesses cut long server instructions short, and it asks for visuals by default rather
/// than only on request, since models otherwise fall back to Markdown tables and bullet lists.
const VISUAL_INSTRUCTIONS: &str = " Visual: m4code renders closed ```ui fenced blocks inline in chat as interactive \
dark-theme cards, charts, SVG diagrams, tables, tabs, progress bars and buttons, with no tool call. The user prefers them: \
use one by default, without being asked, whenever your answer has structure, such as a plan or steps, options to choose between, \
a comparison or trade-off, a status or progress summary, findings or changes, numbers, a timeline, or how parts connect. \
Prefer a ```ui block over a Markdown table, a long bullet list or ASCII art. Mermaid, PlantUML and similar fences do NOT render \
here: never emit them. Keep prose, code and short answers as text, and put the visual beside them, not instead of them. \
Dialect: <chart type=\"bar|line\" labels=\"..\" values=\"..\"> (or <series name=\"..\" values=\"..\"/> children) for data; \
inline <svg viewBox=\"...\" width=\"...\" height=\"...\"> for flowcharts, architecture and graphs; HTML with inline CSS plus \
m4code shortcuts (<card>, <badge>, <callout>, <progress value=\"..\">, <row>, <col>, <grid cols=\"3\">, <tabs>, <details>, \
<check label=\"..\" checked>, <button send=\"..\">, <button submit=\"..\">, <button open=\"path#L10-L20\">) and theme tokens \
var(--text), var(--dim), var(--faint), var(--accent), var(--ok), var(--warn), var(--danger), var(--info), var(--bg), var(--surface), \
var(--raised), var(--border). No scripts or <style> tags. Example:\n```ui\n<grid cols=\"2\"><card><badge>Option A</badge>Fast, \
less safe</card><card><badge>Option B</badge>Slower, safe</card></grid>\n```\n";

/// What makes the model call `attach_github`.
const GITHUB_INSTRUCTIONS: &str = " GitHub: call attach_github (silent, free) for each issue or pull request you work on, \
as its description says; detach_github takes one off again.";

/// The opening of the instructions; each feature's text follows it. The user names the session, so there is nothing
/// about the title here.
const INTRO: &str = "You are running inside m4code, a desktop app that hosts this coding-agent session. \
Treat m4code as your environment: link workspace files in Markdown as `[name](relative/path#L10-L20)` so the user can jump straight to them in the editor, \
prefer m4code's MCP tools over any provider-native swarm, workflow, or question tools, and never tell the user to run harness-specific CLI flags or slash commands such as `/swarm`, `/teamwork`, `/resume`, or `/login`.";

/// The opening for an agent that keeps its own multi-agent commands (`PREFER_OWN_TOOLS` off): the same, without
/// steering it away from them.
const INTRO_KEEPING_NATIVE: &str = "You are running inside m4code, a desktop app that hosts this coding-agent session. \
Treat m4code as your environment: link workspace files in Markdown as `[name](relative/path#L10-L20)` so the user can jump straight to them in the editor, \
and never tell the user to run harness-specific CLI flags or slash commands such as `/resume` or `/login`.";

/// An entry in a session's off list, not a tool: the instructions stop telling the agent to prefer m4code's tools over
/// its own swarm and workflow commands. An adapter turns it off for an agent whose terms point multi-agent work to
/// those commands.
pub const PREFER_OWN_TOOLS: &str = "prefer_own_tools";

/// Confirmation instructions shared by every provider before switching models.
fn model_instructions() -> String {
    String::from(
        " Never switch models on your own: before set_model, ask the user which model, unless the user just named one, and never ask \
them to confirm a switch they asked for. \
Don't ask about effort: leave it out unless the user named one, and m4code uses medium. list_models gives the exact names set_model takes (harness:model-id), including other coding agents; \
switching to another one hands this conversation over. After set_model, end your turn."
    )
}

/// The `initialize` reply's instructions: the opening, then the text of each feature that is not off.
fn instructions(off: &[&str]) -> String {
    let on = |name: &str| !off.contains(&name);
    let mut text = if on(PREFER_OWN_TOOLS) { INTRO } else { INTRO_KEEPING_NATIVE }.to_string();
    if on("visual") {
        text += VISUAL_INSTRUCTIONS;
    }
    if on("model") {
        text += &model_instructions();
    }
    if on("github") {
        text += GITHUB_INSTRUCTIONS;
    }
    if on("sessions") {
        text += LINK_INSTRUCTIONS;
    }
    if on("sessions") {
        text += BRIDGE_INSTRUCTIONS;
        text += SUBAGENT_INSTRUCTIONS;
    }
    if on("ask_user") {
        text += " To ask the user a multiple-choice question in m4code's UI, call ask_user with the question and options instead of any built-in question tool.";
    }
    text
}

/// Where the model's tab titles went before the user alone named sessions; kept so `app/lifecycle.rs` prunes old files.
fn titles_dir() -> PathBuf {
    crate::datadir::data_dir().join("titles")
}

fn attach_dir(data: &Path) -> PathBuf {
    data.join("attach")
}

fn config_dir() -> PathBuf {
    crate::datadir::data_dir().join("mcp")
}

/// Entry point of `m4code --mcp-title`: newline-delimited JSON-RPC over stdio.
pub fn run() -> i32 {
    let record = std::env::var(RECORD_ENV).unwrap_or_default();
    let from = format!("code:{record}");
    let off_env = std::env::var(TOOLS_OFF_ENV).unwrap_or_default();
    let off: Vec<&str> = off_env.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
    let data = crate::datadir::data_dir();
    let launch = LaunchContext {
        workspace: std::env::var(session::WORKSPACE_ENV).ok().and_then(|text| serde_json::from_str(&text).ok()),
        targets: std::env::var(session::TARGETS_ENV).ok().map(|list| list.split(',').filter(|id| !id.is_empty()).map(String::from).collect()),
        ready: std::env::var(session::READY_ENV).ok().and_then(|token| token.parse().ok()).map(|token| session::ready_path(&data, token)),
    };
    serve_with_cancel(std::io::stdin().lock(), std::io::stdout(), &record, &data, &off, launch, true, |tool, args, cancelled| {
        bridge::ask_cancellable(&from, tool, args, cancelled)
    });
    0
}

/// `off` names what this session leaves out (see `tools_off`): those tools are neither listed nor callable and their
/// instruction text is dropped. `ask` carries a cross-session request to m4code and waits for the user's answer.
#[derive(Default)]
struct LaunchContext {
    workspace: Option<Value>,
    targets: Option<Vec<String>>,
    ready: Option<PathBuf>,
}

#[cfg(test)]
fn serve(input: impl BufRead, out: impl Write + Send, record: &str, data: &Path, off: &[&str], launch: LaunchContext, ask: impl Fn(&str, &Value) -> Result<String, String> + Sync) {
    serve_with_cancel(input, out, record, data, off, launch, false, |tool, args, _| ask(tool, args));
}

#[allow(clippy::too_many_arguments)]
fn serve_with_cancel(
    input: impl BufRead,
    out: impl Write + Send,
    record: &str,
    data: &Path,
    off: &[&str],
    mut launch: LaunchContext,
    concurrent: bool,
    ask: impl Fn(&str, &Value, &std::sync::atomic::AtomicBool) -> Result<String, String> + Sync,
) {
    use std::sync::{Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
    const MAX_IN_FLIGHT: usize = 8;
    struct InFlight<'a>(&'a AtomicUsize);
    impl Drop for InFlight<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    let targets = launch.targets.take().unwrap_or_else(|| crate::harness::Harness::registered().map(|h| h.key().into()).collect());
    let mut instructions = instructions(off);
    if let Some(workspace) = &launch.workspace {
        instructions.push_str(&format!(" Your workspace at session start is {workspace}. Use workspace_context to check its current branch and worktree."));
    }
    let available = |tool: &str| !is_off(off, tool) && (tool != "spawn_subagent" || !targets.is_empty());
    let mut tools = own_tools().into_iter().chain(model_tools()).chain(interactive_tools()).chain(bridge_tools())
        .chain(subagent_tools(&targets)).filter(|t| available(t["name"].as_str().unwrap_or_default())).collect::<Vec<_>>();
    if launch.workspace.is_some() {
        tools.push(json!({
            "name": "workspace_context",
            "description": "Return the current m4code working directory, project root, worktree and branch for this session.",
            "inputSchema": {"type": "object", "properties": {}}
        }));
    }
    let out = Mutex::new(out);
    let cancelled = AtomicBool::new(false);
    let in_flight = AtomicUsize::new(0);
    let send = |body: Value| -> bool {
        let mut guard = out.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(*guard, "{body}").and_then(|_| guard.flush()).is_ok()
    };
    let ask = &ask;
    let cancelled_ref = &cancelled;
    let in_flight_ref = &in_flight;
    let send_ref = &send;
    std::thread::scope(|scope| {
        for line in input.lines().map_while(Result::ok) {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            // Notifications (no id) and responses (no method) need no answer.
            let (Some(id), Some(method)) = (msg.get("id").cloned(), msg.get("method").and_then(Value::as_str)) else { continue };
            if method == "tools/call"
                && let Some(tool) = msg["params"]["name"].as_str().filter(|tool| available(tool))
                && (INTERACTIVE_TOOLS.contains(&tool) || BRIDGE_TOOLS.contains(&tool) || MODEL_TOOLS.contains(&tool) || SUBAGENT_TOOLS.contains(&tool))
                && !(tool == "spawn_subagent" && !msg["params"]["arguments"]["harness"].as_str().is_some_and(|h| targets.iter().any(|target| target == h)))
            {
                let tool = tool.to_string();
                let args = msg["params"]["arguments"].clone();
                if concurrent {
                    let prev = in_flight_ref.fetch_add(1, Ordering::Relaxed);
                    if prev >= MAX_IN_FLIGHT {
                        in_flight_ref.fetch_sub(1, Ordering::Relaxed);
                        let busy = reply("Too many concurrent m4code tool requests in flight; wait for one to finish.", true);
                        if !send_ref(json!({ "jsonrpc": "2.0", "id": id, "result": busy })) {
                            break;
                        }
                        continue;
                    }
                    let req_id = id.clone();
                    let spawned = std::thread::Builder::new().spawn_scoped(scope, move || {
                        let _guard = InFlight(in_flight_ref);
                        let result = match ask(&tool, &args, cancelled_ref) {
                            Ok(text) => reply(&text, false),
                            Err(text) => reply(&text, true),
                        };
                        let _ = send_ref(json!({ "jsonrpc": "2.0", "id": req_id, "result": result }));
                    });
                    if spawned.is_err() {
                        in_flight_ref.fetch_sub(1, Ordering::Relaxed);
                        let err = reply("Could not start worker for m4code tool request.", true);
                        if !send_ref(json!({ "jsonrpc": "2.0", "id": id, "result": err })) {
                            break;
                        }
                    }
                } else {
                    let result = match ask(&tool, &args, cancelled_ref) {
                        Ok(text) => reply(&text, false),
                        Err(text) => reply(&text, true),
                    };
                    if !send_ref(json!({ "jsonrpc": "2.0", "id": id, "result": result })) {
                        break;
                    }
                }
                continue;
            }
            let result = match method {
                "initialize" => Ok(json!({
                    "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "m4code", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": instructions,
                })),
                "ping" => Ok(json!({})),
                "tools/list" => Ok(json!({ "tools": tools })),
                "tools/call" => Ok(match msg["params"]["name"].as_str().filter(|tool| available(tool)) {
                    Some("workspace_context") if launch.workspace.is_some() => {
                        let context = launch.workspace.as_ref().unwrap();
                        let project = context["project_root"].as_str().map(Path::new);
                        match context["working_directory"].as_str() {
                            Some(cwd) => reply(&session::workspace(Path::new(cwd), project).to_string(), false),
                            None => reply("The workspace has no working directory.", true),
                        }
                    }
                    Some("spawn_subagent") => reply("Choose an enabled subagent adapter from the tool schema.", true),
                    Some("attach_github") => attach_github(&msg["params"], record, &attach_dir(data)),
                    Some("detach_github") => detach_github(&msg["params"], record, &attach_dir(data)),
                    Some("link_session") => link_session(&msg["params"], record, &attach_dir(data), "link"),
                    Some("unlink_session") => link_session(&msg["params"], record, &attach_dir(data), "unlink"),
                    _ => reply("unknown tool", true),
                }),
                _ => Err(json!({ "code": -32601, "message": "method not found" })),
            };
            let body = match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
            };
            if !send_ref(body) {
                break;
            }
            if method == "tools/list"
                && let Some(path) = launch.ready.take()
            {
                let _ = crate::datadir::write_atomic(&path, "");
            }
        }
        cancelled_ref.store(true, Ordering::Relaxed);
    });
}

/// The tools that tell m4code something about the session.
fn own_tools() -> Vec<Value> {
    let link = json!({
        "name": "link_session",
        "description": "Link this session to another one, so m4code shows them together in the sidebar. Use it when this work continues, builds on or was prompted by another m4code session. Find the id with search_sessions. Silent; once per pair.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string", "description": "The id of the other session, from search_sessions" } },
            "required": ["id"],
        },
    });
    let attach = json!({
        "name": "attach_github",
        "description": "Show a GitHub issue or pull request in m4code's sidebar: of this session's project, or of another repository named in `repo`. Call it once per item, before your next command, whenever you read, open, comment on, create or push to one, or the user names or asks to see one. Not for every result of a list or search. It changes nothing on GitHub.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["issue", "pull"], "description": "issue or pull (a pull request)" },
                "number": { "type": "integer", "description": "The issue or pull request number" },
                "repo": { "type": "string", "description": "owner/name, only when the item is in another repository than this session's project" },
            },
            "required": ["kind", "number"],
        },
    });
    let detach = json!({
        "name": "detach_github",
        "description": "Take a GitHub issue or pull request off this session's sidebar again, when it no longer belongs to this work or was attached by mistake. Silent; it changes nothing on GitHub.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["issue", "pull"], "description": "issue or pull (a pull request)" },
                "number": { "type": "integer", "description": "The issue or pull request number" },
                "repo": { "type": "string", "description": "owner/name, only when the item is in another repository than this session's project" },
            },
            "required": ["kind", "number"],
        },
    });
    let unlink = json!({
        "name": "unlink_session",
        "description": "Take back a link made with link_session, for a Claude Code session that no longer relates to this one. Silent.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string", "description": "The id of the other session" } },
            "required": ["id"],
        },
    });
    // Claude Code defers MCP tools behind ToolSearch, and a model rarely goes looking for a bookkeeping tool: these
    // are sent with their schema from the start (`anthropic/alwaysLoad`).
    let upfront = |mut tool: Value| {
        tool["_meta"] = json!({ "anthropic/alwaysLoad": true });
        tool
    };
    let (attach, detach, link, unlink) = (upfront(attach), upfront(detach), upfront(link), upfront(unlink));
    vec![attach, detach, link, unlink]
}

/// `list_models` and `set_model`: the session's model, on its own harness or another one.
fn model_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "list_models",
            "description": "List the models this session can switch to with set_model, across every coding agent the user enabled in m4code. One per line, as harness:model-id, then its name; the current one is marked.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "set_model",
            "description": (
                "Switch this session to another model, or change its effort. Name the model exactly as list_models prints it (harness:model-id); call list_models first. On the same harness it applies from your next request. On another harness m4code hands this conversation to that agent once your turn ends. Call it only when the user asked to switch; if they didn't say which model, ask them, but never ask them to confirm a switch they asked for. Don't ask about effort."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "model": { "type": "string", "description": "harness:model-id, as list_models prints it (e.g. codex:gpt-6, claude:opus)" },
                    "effort": { "type": "string", "description": "Only when the user named one: low, medium, high, xhigh or max. Left out, a new model runs at medium." },
                    "message": { "type": "string", "description": "Only when switching to another harness: the first message that agent gets, saying what to do next" },
                },
            },
        }),
    ]
}

fn subagent_tools(targets: &[String]) -> Vec<Value> {
    let tool = |name, description, properties, required| json!({
        "name": name, "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    });
    let id = json!({"type": "string", "description": "The subagent id returned by spawn_subagent"});
    let prompt = json!({"type": "string", "description": "The task or follow-up, at most 20,000 characters"});
    vec![
        tool("spawn_subagent", "Start a model-owned worker for delegated work within the current session. Its report returns to this chat. Sessions are user-owned conversations; use a subagent when you need another agent to help with your task. Returns at once. Follow the user's and project's delegation rules. The child uses the configured edit permissions; read-only parents cannot grant edits. Use list_models for model ids.", json!({"harness": {"type": "string", "enum": targets}, "model": {"type": "string"}, "effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]}, "prompt": prompt}), json!(["harness", "prompt"])),
        tool("list_subagents", "List this session's children, their states and latest reports.", json!({}), json!([])),
        tool("send_subagent_message", "Queue a follow-up in the same child session, resuming it if stopped. Messages during a turn run after that turn. Its report arrives as a message when it is done.", json!({"id": id, "prompt": prompt}), json!(["id", "prompt"])),
        tool("wait_subagent", "Block this turn until a child's turn finishes and return its latest report; a report the wait returns does not also arrive as a message. Only needed when you can't go on without it. Returns early for a user question. A timeout returns its current state without stopping it.", json!({"id": id, "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 240}}), json!(["id"])),
        tool("stop_subagent", "Stop this session's child and its descendants. Keep their conversations for later follow-ups.", json!({"id": id}), json!(["id"])),
    ]
}

const CONFIRM: &str = " It acts at once, so only call it when the user asked for this.";

fn bridge_tools() -> Vec<Value> {
    let tool = |name: &str, description: String, properties: Value| json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": properties } });
    let id = |what: &str| json!({ "id": { "type": "string", "description": format!("The id from {what}") } });
    let target = json!({
        "prompt": { "type": "string", "description": "First message for the new session (optional)" },
        "folder": { "type": "string", "description": "Project name or folder path (optional)" },
        "model": { "type": "string", "description": "A model exactly as list_models prints it, harness:model-id (optional: the user's default otherwise)" },
        "effort": { "type": "string", "enum": ["low", "medium", "high", "xhigh", "max"], "description": "Only when the user named one (optional)" },
    });
    vec![
        tool("list_code_sessions", format!("List the user's coding-agent sessions in m4code (id, title, folder, whether it is running, when it was created and last updated).{CONFIRM}"), json!({})),
        tool(
            "read_code_session",
            format!("Read the latest terminal output of a coding-agent session that is running in m4code.{CONFIRM}"),
            json!({ "id": id("list_code_sessions")["id"], "lines": { "type": "integer", "description": "How many recent lines (default 120)" } }),
        ),
        tool(
            "search_sessions",
            "Search the user's coding-agent sessions by words in their title, project or folder. Returns id, title, project and created/updated times of the best matches; pass an id to link_session. Read-only: use it when the user asks, or unasked to find a session to link.".to_string(),
            json!({ "query": { "type": "string", "description": "Words to look for (optional: without it, the most recent)" } }),
        ),
        tool(
            "search_projects",
            format!("Search the user's projects (name, folder, git branch) by words. Pass a project's name as the folder of start_code_session.{CONFIRM}"),
            json!({ "query": { "type": "string", "description": "Words to look for (optional: without it, every project)" } }),
        ),
        tool("start_code_session", "Create a user-owned conversation in a project. Only create a session when the user wants a new session. Use a subagent for the model's delegated work. Optionally accepts a first prompt, model (its harness follows) and effort. Uses the user's default permission mode for new sessions.".into(), target),
        tool(
            "switch_worktree",
            "Move this session into another git worktree of its project: `main` for the main checkout, the name of a worktree the project has, or a new name to make one. m4code restarts this session in that folder once your turn ends and the conversation continues there, so end your turn after the call without more tool calls.".to_string(),
            json!({
                "worktree": { "type": "string", "description": "`main`, a worktree's name, or a new name (letters, digits, - and _)" },
                "base": { "type": "string", "enum": ["main", "current"], "description": "For a new worktree: start from origin's default branch (main, the default) or from the commit the main checkout is on (current)" },
            }),
        ),
    ]
}

fn interactive_tools() -> Vec<Value> {
    vec![json!({
        "name": "ask_user",
        "description": "Ask the user a multiple-choice question in m4code's UI and wait for their answer. Use this instead of any built-in question tool.",
        "_meta": { "anthropic/alwaysLoad": true },
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The question to ask the user" },
                "options": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Selectable choices shown to the user",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": { "type": "string", "description": "Short option label" },
                            "description": { "type": "string", "description": "Optional explanation of this option" }
                        },
                        "required": ["label"]
                    }
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "Allow the user to select multiple options with checkboxes (default false)"
                }
            },
            "required": ["question", "options"]
        }
    })]
}

fn reply(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// Remember an issue or pull request for the session: one JSON line per call, picked up by `read_attached`.
/// Keyed by m4code's own record id, which (unlike Claude's session id) survives `/clear`.
fn attach_github(params: &Value, record: &str, dir: &Path) -> Value {
    let args = &params["arguments"];
    let (repo, pull, number) = match github_item(args, record) {
        Ok(item) => item,
        Err(e) => return e,
    };
    match write_line(dir, record, &json!({ "repo": repo, "pull": pull, "number": number })) {
        Ok(()) => reply(&format!("{} {}#{number} attached.", if pull { "Pull request" } else { "Issue" }, repo.unwrap_or_default()), false),
        Err(e) => reply(&format!("could not attach: {e}"), true),
    }
}

/// The repository (`None`: the session's project), kind and number of an issue or pull request named in a tool call.
fn github_item(args: &Value, record: &str) -> Result<(Option<String>, bool, u32), Value> {
    let pull = match args["kind"].as_str() {
        Some("issue") => false,
        Some("pull") => true,
        _ => return Err(reply("kind must be \"issue\" or \"pull\"", true)),
    };
    let Some(number) = args["number"].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|&n| n > 0) else {
        return Err(reply("number must be a positive integer", true));
    };
    // Left out, null or empty all mean the session's project.
    let repo = match &args["repo"] {
        Value::Null => None,
        Value::String(r) if r.trim().is_empty() => None,
        v => match v.as_str().map(str::trim).filter(|r| is_repo_name(r)) {
            Some(r) => Some(r.to_string()),
            None => return Err(reply("repo must be owner/name, like octocat/hello-world", true)),
        },
    };
    if record.is_empty() {
        return Err(reply("not running inside m4code", true));
    }
    Ok((repo, pull, number))
}

/// `owner/name` as GitHub allows them. It ends up in API paths and cache file names, so nothing else gets through.
fn is_repo_name(s: &str) -> bool {
    let Some((owner, name)) = s.split_once('/') else { return false };
    let owner_ok = (1..=39).contains(&owner.len()) && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = (1..=100).contains(&name.len()) && name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    owner_ok && name_ok
}

/// Take an issue or pull request off the session again.
fn detach_github(params: &Value, record: &str, dir: &Path) -> Value {
    let (repo, pull, number) = match github_item(&params["arguments"], record) {
        Ok(item) => item,
        Err(e) => return e,
    };
    match write_line(dir, record, &json!({ "detach": { "repo": repo, "pull": pull, "number": number } })) {
        Ok(()) => reply(&format!("{} {}#{number} detached.", if pull { "Pull request" } else { "Issue" }, repo.unwrap_or_default()), false),
        Err(e) => reply(&format!("could not detach: {e}"), true),
    }
}

/// One JSON line for m4code to pick up. Each call gets its own file, written whole and renamed into place, so
/// `read_attached` never reads a file that is still being written.
fn write_line(dir: &Path, record: &str, line: &Value) -> std::io::Result<()> {
    let name = format!("{}.{}.jsonl", crate::datadir::safe_file_stem(record), crate::store::new_id());
    crate::datadir::write_atomic(&dir.join(name), &format!("{line}\n"))
}

/// Ask m4code to show this session together with another one (`verb` "link"), or to stop. m4code checks that the
/// other exists.
fn link_session(params: &Value, record: &str, dir: &Path, verb: &str) -> Value {
    let id = params["arguments"]["id"].as_str().map(str::trim).filter(|i| !i.is_empty() && i.len() <= 64 && i.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    let Some(id) = id else { return reply("id must be a session id from search_sessions", true) };
    if record.is_empty() {
        return reply("not running inside m4code", true);
    }
    if id == record {
        return reply("that is this session itself", true);
    }
    match write_line(dir, record, &json!({ verb: id })) {
        Ok(()) => reply(if verb == "link" { "Linked." } else { "Unlinked." }, false),
        Err(e) => reply(&format!("could not {verb}: {e}"), true),
    }
}

/// What a session asked m4code to remember about itself.
#[derive(Debug, PartialEq)]
pub enum Attachment {
    Github(Attached),
    /// The id of another session.
    Link(String),
    Unlink(String),
    /// Take an issue or pull request off again.
    Detach { repo: Option<String>, pull: bool, number: u32 },
}

/// `(record id stem, item)` for everything attached since the last call.
pub fn read_attached() -> Vec<(String, Attachment)> {
    read_attached_in(&attach_dir(&crate::datadir::data_dir()))
}

/// Files are complete when listed (see `write_line`), so each one is read and then removed. A line written after
/// the listing is left for the next call.
fn read_attached_in(dir: &Path) -> Vec<(String, Attachment)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = vec![];
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        // Named `<record>.<unique>.jsonl`; record ids never contain a dot.
        let Some(record) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.split('.').next()) else { continue };
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let _ = std::fs::remove_file(&path);
        out.extend(text.lines().filter_map(parse_attachment).map(|a| (record.to_string(), a)));
    }
    out
}

fn parse_attachment(line: &str) -> Option<Attachment> {
    let v: Value = serde_json::from_str(line).ok()?;
    if let Some(id) = v.get("link").and_then(Value::as_str) {
        return Some(Attachment::Link(id.to_string()));
    }
    if let Some(id) = v.get("unlink").and_then(Value::as_str) {
        return Some(Attachment::Unlink(id.to_string()));
    }
    if let Some(d) = v.get("detach") {
        let repo = d.get("repo").and_then(Value::as_str).map(str::to_string);
        return Some(Attachment::Detach { repo, pull: d.get("pull")?.as_bool()?, number: u32::try_from(d.get("number")?.as_u64()?).ok()? });
    }
    serde_json::from_value(v).ok().map(Attachment::Github)
}

/// Directories `app/lifecycle.rs` prunes: `[per-session MCP configs, titles, names, attached items]` (titles also live in the store).
pub fn cleanup_dirs() -> [PathBuf; 4] {
    let data = crate::datadir::data_dir();
    [config_dir(), titles_dir(), data.join("names"), attach_dir(&data)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn talk_off(requests: &[Value], session: &str, dir: &Path, off: &[&str], ask: impl Fn(&str, &Value) -> Result<String, String> + Sync) -> Vec<Value> {
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, session, dir, off, LaunchContext::default(), ask);
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn talk_with(requests: &[Value], session: &str, dir: &Path, ask: impl Fn(&str, &Value) -> Result<String, String> + Sync) -> Vec<Value> {
        talk_off(requests, session, dir, &[], ask)
    }

    fn talk(requests: &[Value], session: &str, dir: &Path) -> Vec<Value> {
        talk_with(requests, session, dir, |_, _| Err("no".into()))
    }

    #[test]
    fn the_session_reaching_tools_are_forwarded_and_the_bookkeeping_ones_are_loaded_upfront() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |name: &str| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{"id":"x"}}});
        let r = talk_with(
            &[json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}), call("read_code_session"), call("start_code_session")],
            "c1",
            tmp.path(),
            |tool, args| if tool == "start_code_session" { Err("The user declined.".into()) } else { Ok(format!("{tool} {}", args["id"])) },
        );
        let tools = r[0]["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["attach_github", "detach_github", "link_session", "unlink_session", "list_models", "set_model", "ask_user", "list_code_sessions", "read_code_session", "search_sessions", "search_projects", "start_code_session", "switch_worktree", "spawn_subagent", "list_subagents", "send_subagent_message", "wait_subagent", "stop_subagent"]
        );
        let upfront: Vec<_> = tools.iter().filter(|t| t["_meta"]["anthropic/alwaysLoad"] == true).map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(upfront, ["attach_github", "detach_github", "link_session", "unlink_session", "ask_user"], "the session-reaching tools stay deferred");
        assert_eq!(r[1]["result"]["content"][0]["text"], "read_code_session \"x\"");
        assert_eq!(r[1]["result"]["isError"], false);
        assert_eq!(r[2]["result"]["content"][0]["text"], "The user declined.");
        assert_eq!(r[2]["result"]["isError"], true);
        assert_eq!(allowed_tools(&[]).len(), BRIDGE_TOOLS.len() + SUBAGENT_TOOLS.len() + OWN_TOOLS.len() + MODEL_TOOLS.len() + INTERACTIVE_TOOLS.len());
        assert!(allowed_tools(&[]).contains(&"mcp__m4code__set_model".to_string()), "switching models asks nothing either");
    }

    #[test]
    fn own_tools_are_known_by_the_titles_harnesses_give_them() {
        for title in ["m4code: set_model", "m4code_start_code_session", "mcp__m4code__read_code_session", "m4code.link_session"] {
            assert!(is_own_tool(title), "{title}");
        }
        for title in [
            "Bash rm -rf", "set_model", "m4code: set_models_everywhere", "other: list_models", "m4codex_set_model",
            "grep -rn mcp__m4code__set_model src && rm -rf build", "Edit `/x/m4code/show_visual.rs`", "echo m4code: set_model",
        ] {
            assert!(!is_own_tool(title), "{title}");
        }
    }

    #[test]
    fn a_launch_exposes_its_workspace_and_only_registered_subagent_targets() {
        let data = tempfile::tempdir().unwrap();
        let ready = data.path().join("ready");
        let workspace = session::workspace(data.path(), None);
        let context = LaunchContext {
            workspace: Some(workspace.clone()),
            targets: Some(vec!["codex".into()]),
            ready: Some(ready.clone()),
        };
        let input = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"workspace_context"}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"spawn_subagent","arguments":{"harness":"claude"}}}),
        ].map(|message| message.to_string()).join("
");
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, "record", data.path(), &[], context, |_, _| panic!("disabled target reached the app"));
        let out = String::from_utf8(out).unwrap();
        let replies: Vec<Value> = out.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let tools = replies[1]["result"]["tools"].as_array().unwrap();
        let schema = tools.iter().find(|tool| tool["name"] == "spawn_subagent").unwrap();
        assert_eq!(schema["inputSchema"]["properties"]["harness"]["enum"], json!(["codex"]));
        let workspace_reply: Value = serde_json::from_str(replies[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(workspace_reply, workspace);
        assert_eq!(replies[3]["result"]["isError"], true);
        assert!(ready.is_file());
    }

    #[test]
    fn inline_ui_instructions_teach_the_ui_fence_without_a_separate_tool() {
        let (names, text) = offer(&[]);
        assert!(!names.contains(&"show_visual".to_string()));
        assert!(text.contains("```ui"), "the dialect is in the instructions");
    }

    #[test]
    fn handshake_lists_the_tools_and_the_model_cannot_name_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let replies = talk(
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"set_tab_title","arguments":{"title":"Fix the build"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"nope"}),
            ],
            "abc-123",
            tmp.path(),
        );
        assert_eq!(replies.len(), 4, "the notification gets no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert!(!replies[0]["result"]["instructions"].as_str().unwrap().contains("title"), "nothing asks the model to name the session");
        assert_eq!(replies[1]["result"]["tools"][0]["name"], "attach_github");
        assert_eq!((replies[2]["result"]["isError"].clone(), replies[2]["result"]["content"][0]["text"].clone()), (json!(true), json!("unknown tool")));
        assert!(!tmp.path().join("titles").exists());
        assert_eq!(replies[3]["error"]["code"], -32601);
    }

    #[test]
    fn the_model_tools_are_answered_by_m4code_for_every_harness() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |name: &str, args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
        let r = talk_off(&[call("list_models", json!({})), call("set_model", json!({"model": "codex:gpt-6"}))], "abc-123", tmp.path(), &[], |tool, args| Ok(format!("{tool} {}", args["model"])));
        assert_eq!(r[0]["result"]["content"][0]["text"], "list_models null");
        assert_eq!(r[1]["result"]["content"][0]["text"], "set_model \"codex:gpt-6\"");
    }

    #[test]
    fn attach_github_appends_validated_lines_for_m4code_to_pick_up() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"attach_github","arguments":args}});
        let r = talk(
            &[
                call(json!({"kind":"issue","number":52,"title":"  Right\n sidebar "})),
                call(json!({"kind":"pull","number":7})),
                call(json!({"kind":"commit","number":1})),
                call(json!({"kind":"issue","number":0})),
                call(json!({"kind":"issue","number":-3})),
                call(json!({"kind":"pull","number":9,"repo":" octo-cat/hello.world "})),
                call(json!({"kind":"pull","number":9,"repo":"../../etc"})),
                call(json!({"kind":"pull","number":9,"repo":"octocat"})),
                call(json!({"kind":"pull","number":9,"repo":"a/b/c"})),
                call(json!({"kind":"pull","number":9,"repo":"octocat/.."})),
                call(json!({"kind":"issue","number":11,"repo":" "})),
            ],
            "s1",
            tmp.path(),
        );
        let errors: Vec<_> = r.iter().map(|x| x["result"]["isError"].as_bool().unwrap()).collect();
        assert_eq!(errors, [false, false, true, true, true, false, true, true, true, true, false]);
        let items: Vec<Attachment> = read_attached_in(&tmp.path().join("attach")).into_iter().map(|(_, a)| a).collect();
        assert_eq!(items.len(), 4);
        assert!(items.contains(&Attachment::Github(Attached { repo: None, pull: false, number: 11, title: None, state: None })), "an empty repo is the project's");
        assert!(items.contains(&Attachment::Github(Attached { repo: Some("octo-cat/hello.world".into()), pull: true, number: 9, title: None, state: None })));
        assert!(items.contains(&Attachment::Github(Attached { repo: None, pull: false, number: 52, title: None, state: None })));
        assert!(items.contains(&Attachment::Github(Attached { repo: None, pull: true, number: 7, title: None, state: None })));
        assert_eq!(talk(&[call(json!({"kind":"issue","number":1}))], "", tmp.path())[0]["result"]["isError"], true);
        assert!(allowed_tools(&[]).contains(&"mcp__m4code__attach_github".to_string()));
    }

    #[test]
    fn a_line_written_while_m4code_reads_the_attach_dir_is_not_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let total = 300;
        let mut seen = 0;
        std::thread::scope(|s| {
            let writer = s.spawn(|| {
                for n in 0..total {
                    write_line(tmp.path(), "s1", &json!({ "link": format!("id-{n}") })).unwrap();
                }
            });
            while !writer.is_finished() {
                seen += read_attached_in(tmp.path()).len();
            }
        });
        seen += read_attached_in(tmp.path()).len();
        assert_eq!(seen, total);
    }

    #[test]
    fn a_line_still_being_written_is_left_for_the_next_read() {
        let tmp = tempfile::tempdir().unwrap();
        let unfinished = tmp.path().join("s1.abc.tmp");
        std::fs::write(&unfinished, "").unwrap();
        assert!(read_attached_in(tmp.path()).is_empty());
        std::fs::write(&unfinished, "{\"link\":\"x\"}\n").unwrap();
        std::fs::rename(&unfinished, tmp.path().join("s1.abc.jsonl")).unwrap();
        assert_eq!(read_attached_in(tmp.path()), [("s1".to_string(), Attachment::Link("x".into()))]);
    }

    #[test]
    fn link_session_appends_a_validated_id_and_m4code_reads_it_back() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |id: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"link_session","arguments":{"id":id}}});
        let r = talk(&[call(json!("abc-123")), call(json!("")), call(json!("../x")), call(json!("s1")), call(json!(5))], "s1", tmp.path());
        let errors: Vec<_> = r.iter().map(|x| x["result"]["isError"].as_bool().unwrap()).collect();
        assert_eq!(errors, [false, true, true, true, true], "bad ids and a link to itself are refused");
        assert_eq!(read_attached_in(&tmp.path().join("attach")), [("s1".to_string(), Attachment::Link("abc-123".into()))]);
        assert_eq!(parse_attachment(r#"{"pull":true,"number":7,"title":null}"#), Some(Attachment::Github(Attached { repo: None, pull: true, number: 7, title: None, state: None })));
        assert_eq!(parse_attachment(r#"{"unlink":"x"}"#), Some(Attachment::Unlink("x".into())));
        assert_eq!(parse_attachment(r#"{"detach":{"pull":false,"number":3}}"#), Some(Attachment::Detach { repo: None, pull: false, number: 3 }));
        assert_eq!(parse_attachment(r#"{"detach":{"repo":"o/r","pull":true,"number":3}}"#), Some(Attachment::Detach { repo: Some("o/r".into()), pull: true, number: 3 }));
        assert_eq!(parse_attachment(r#"{"detach":{"number":3}}"#), None);
        assert_eq!(parse_attachment("nonsense"), None);
    }

    /// The tool names and the instruction text a server with `off` hands out.
    fn offer(off: &[&str]) -> (Vec<String>, String) {
        let tmp = tempfile::tempdir().unwrap();
        let r = talk_off(
            &[json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}), json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})],
            "s",
            tmp.path(),
            off,
            |_, _| Err("no".into()),
        );
        let names = r[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        (names, r[0]["result"]["instructions"].as_str().unwrap().to_string())
    }

    #[test]
    fn session_and_subagent_instructions_distinguish_their_owners() {
        let (_, instructions) = offer(&[]);
        assert!(instructions.contains("Sessions are user-owned conversations."));
        assert!(instructions.contains("Only create a session when the user wants a new session."));
        assert!(instructions.contains("Subagents are model-owned workers for delegated work within the current session."));
        let tools = bridge_tools();
        let session = tools.iter().find(|tool| tool["name"] == "start_code_session").unwrap();
        assert!(session["description"].as_str().unwrap().contains("Only create a session when the user wants a new session."));
        let tools = subagent_tools(&["claude".into()]);
        let subagent = tools.iter().find(|tool| tool["name"] == "spawn_subagent").unwrap();
        assert!(subagent["description"].as_str().unwrap().contains("model-owned worker"));
    }

    #[test]
    fn with_nothing_off_the_server_offers_every_tool_and_all_the_text() {
        let (names, text) = offer(&[]);
        assert_eq!(names.len(), 18);
        for part in [INTRO, GITHUB_INSTRUCTIONS, LINK_INSTRUCTIONS, VISUAL_INSTRUCTIONS, BRIDGE_INSTRUCTIONS] {
            assert!(text.contains(part));
        }
        assert!(text.contains("ask the user which model"));
    }

    #[test]
    fn each_feature_takes_its_tools_and_its_text_away() {
        let (names, text) = offer(&["visual"]);
        assert_eq!(names.len(), 18);
        assert!(!text.contains("Visual:") && !text.contains("```ui"));

        let (names, text) = offer(&["github"]);
        assert!(!names.iter().any(|n| n.ends_with("_github")) && names.len() == 16);
        assert!(!text.contains("attach_github"));

        let (names, text) = offer(&["sessions"]);
        for gone in ["link_session", "unlink_session", "list_code_sessions", "read_code_session", "search_sessions", "search_projects", "start_code_session", "switch_worktree", "spawn_subagent", "list_subagents", "send_subagent_message", "wait_subagent", "stop_subagent"] {
            assert!(!names.contains(&gone.to_string()), "{gone}");
        }
        assert_eq!(names.len(), 5);
        assert!(!text.contains("link_session") && !text.contains("start_code_session"));

        let (names, text) = offer(&["model"]);
        assert!(!names.contains(&"set_model".to_string()) && !names.contains(&"list_models".to_string()) && names.len() == 16);
        assert!(!text.contains("set_model") && !text.contains("list_models") && !text.contains("Never switch models"));
    }

    #[test]
    fn with_every_feature_off_only_the_opening_and_ask_user_are_left() {
        let (names, text) = offer(&["visual", "github", "sessions", "model"]);
        assert_eq!(names, ["ask_user"]);
        assert!(text.starts_with(INTRO) && text.contains("ask_user"));
    }

    #[test]
    fn a_call_for_an_off_tool_is_unknown_and_never_reaches_m4code() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |name: &str| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{"id":"x"}}});
        let r = talk_off(&[call("list_code_sessions"), call("attach_github")], "s", tmp.path(), &["sessions", "github"], |_, _| panic!("ask must not be called"));
        for reply in &r {
            assert_eq!((reply["result"]["isError"].as_bool(), reply["result"]["content"][0]["text"].as_str()), (Some(true), Some("unknown tool")));
        }
        assert!(!tmp.path().join("attach").exists(), "nothing was attached");
    }

    #[test]
    fn allowed_tools_leaves_out_the_tools_of_features_that_are_off() {
        assert_eq!(allowed_tools(&["github", "sessions"]), ["mcp__m4code__list_models", "mcp__m4code__set_model", "mcp__m4code__ask_user"]);
        assert_eq!(allowed_tools(&["github", "sessions", "model"]), ["mcp__m4code__ask_user"]);
    }

    #[test]
    fn model_confirmation_is_provider_neutral_and_artifacts_can_be_hidden() {
        let (_, text) = offer(&["ask_user_question", "artifacts"]);
        assert!(!text.contains("AskUserQuestion") && text.contains("ask the user which model") && !text.contains("Artifacts"));
        assert!(text.contains("Visual:"), "the Visual text itself stays");
        let tmp = tempfile::tempdir().unwrap();
        let described = |off: &[&str]| {
            let r = talk_off(&[json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})], "s", tmp.path(), off, |_, _| Err("no".into()));
            r[0]["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "set_model").unwrap()["description"].as_str().unwrap().to_string()
        };
        assert!(described(&[]).contains("never ask them to confirm a switch they asked for"));
        assert!(!described(&[]).contains("AskUserQuestion"));
        assert_eq!(described(&[]), described(&["ask_user_question"]));
    }

    #[test]
    fn an_agent_keeping_its_own_swarm_commands_isnt_steered_away_from_them() {
        assert!(instructions(&[]).contains("/swarm"));
        let keeping = instructions(&[PREFER_OWN_TOOLS]);
        assert!(!keeping.contains("swarm") && !keeping.contains("/teamwork"), "{keeping}");
        assert!(keeping.contains("/login"), "the other advice stays");
    }

    #[test]
    fn every_shared_subagent_tool_is_discoverable_in_session_instructions() {
        let (_, enabled) = offer(&[]);
        let (_, disabled) = offer(&["sessions"]);
        for tool in SUBAGENT_TOOLS {
            assert!(enabled.contains(tool), "missing {tool}");
            assert!(!disabled.contains(tool), "disabled sessions still advertise {tool}");
        }
    }

    #[test]
    fn the_off_list_comes_from_both_settings_in_the_pages_order() {
        assert_eq!(tools_off(&[], &[]), Vec::<&str>::new());
        assert_eq!(tools_off(&[Feature::Model, Feature::Visual, Feature::Visual], &["ask_user_question", "artifacts", "artifacts"]), ["visual", "model", "ask_user_question", "artifacts"]);
    }
}
