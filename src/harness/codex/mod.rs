//! OpenAI Codex, through Zed's `codex-acp`, the same server Codex subagents use. It builds on Codex's own Rust core
//! rather than the `codex` binary, but reads the user's `~/.codex` config and `codex login`.

pub mod limits;

use super::limits::Pool;
use super::subagents::Provider;
use crate::panels::logos;
use super::subagents::Agent;

use super::{Adapter, Reopen};
use serde_json::{Map, Value, json};

pub(crate) const CONFIG_ENV: &str = "CODEX_CONFIG";

pub(crate) fn m4code_mcp_config(inherited: Option<&str>, command: &std::path::Path, env: &[(&str, String)]) -> Result<String, String> {
    let mut config: Map<String, Value> = match inherited.filter(|text| !text.is_empty()) {
        Some(text) => serde_json::from_str(text).map_err(|_| format!("{CONFIG_ENV} must be a JSON object."))?,
        None => Map::new(),
    };
    let servers = config.entry("mcp_servers").or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| format!("{CONFIG_ENV} mcp_servers must be a JSON object."))?;
    // Codex can build the first turn's tool list before optional MCP servers finish starting.
    servers.insert("m4code".into(), json!({
        "command": command,
        "args": [crate::mcp::FLAG],
        "env": env.iter().map(|(name, value)| (name.to_string(), json!(value))).collect::<Map<_, _>>(),
        "required": true,
    }));
    Ok(Value::Object(config).to_string())
}

use super::{AsyncAsk, AsyncOption, AsyncQuestion};

fn parse_async_titled_options(entries: &[Value], has_note: bool) -> Vec<AsyncOption> {
    entries
        .iter()
        .filter_map(|option| {
            let label = option["const"].as_str()?;
            let is_placeholder = label == "None of the above"
                && has_note
                && option["description"] == "Provide a different answer in the note field.";
            if is_placeholder {
                return None;
            }
            let description = option["description"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
            Some(AsyncOption { label: label.to_string(), description })
        })
        .collect()
}

fn parse_async_enum_options(entries: &[Value], names: Option<&Vec<Value>>) -> Vec<AsyncOption> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let val = v.as_str()?;
            let label = names.and_then(|n| n.get(i)).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).unwrap_or(val);
            Some(AsyncOption { label: label.to_string(), description: None })
        })
        .collect()
}

fn async_questions(params: &Value) -> Option<Vec<AsyncQuestion>> {
    if params["mode"] != "form" || params.pointer("/_meta/codex/delivery").and_then(Value::as_str) != Some("async") {
        return None;
    }
    let properties = params.pointer("/requestedSchema/properties")?.as_object()?;
    let questions: Vec<_> = (0..properties.len()).map_while(|index| {
        let field = properties.get(&format!("question_{index}"))?;
        let title = field["title"].as_str()?.to_string();
        let has_note = properties.contains_key(&format!("question_{index}_note"));
        let multi_select = field["type"].as_str() == Some("array");
        let options = if multi_select {
            let items = &field["items"];
            if let Some(any_of) = items["anyOf"].as_array() {
                parse_async_titled_options(any_of, has_note)
            } else if let Some(enums) = items["enum"].as_array() {
                parse_async_enum_options(enums, items["enumNames"].as_array())
            } else {
                vec![]
            }
        } else if let Some(one_of) = field["oneOf"].as_array() {
            parse_async_titled_options(one_of, has_note)
        } else if let Some(enums) = field["enum"].as_array() {
            parse_async_enum_options(enums, field["enumNames"].as_array())
        } else {
            vec![]
        };
        Some(AsyncQuestion { title, options, multi_select })
    }).collect();
    (!questions.is_empty()).then_some(questions)
}

fn async_question_text(questions: &[AsyncQuestion]) -> String {
    questions.iter().map(|question| {
        std::iter::once(question.title.clone()).chain(question.options.iter().map(|option| format!("- {}", option.label)))
            .collect::<Vec<_>>().join("\n")
    }).collect::<Vec<_>>().join("\n\n")
}

fn decode_async_ask(params: &Value) -> Option<AsyncAsk> {
    let questions = async_questions(params)?;
    let echoed_text = async_question_text(&questions);
    Some(AsyncAsk { questions, echoed_text })
}

fn ignore_update(update: &Value) -> bool {
    if update["sessionUpdate"] != "agent_message_chunk" {
        return false;
    }
    let text = update.pointer("/content/text").and_then(Value::as_str).unwrap_or_default().trim();
    text.starts_with("Warning: This session was recorded with model ")
        && text.contains(" but is resuming with ")
        && text.contains("Consider switching back to ")
}
/// A text-generation run: no web search, shell, apps, hooks or subagents, and read-only.
fn text_generation_launch(launch: &mut super::acp::Launch, _: &std::path::Path) -> Result<Value, String> {
    let mut config = super::text_generation::inline_config("CODEX_CONFIG")?;
    config.insert("web_search".into(), json!("disabled"));
    let features = config.entry("features").or_insert_with(|| json!({}));
    let features = features.as_object_mut().ok_or("Codex features configuration must be an object.")?;
    for feature in ["shell_tool", "apps", "hooks", "multi_agent"] {
        features.insert(feature.into(), json!(false));
    }
    launch.env.push(("CODEX_CONFIG".into(), json!(config).to_string()));
    launch.env.push(("INITIAL_AGENT_MODE".into(), "read-only".into()));
    Ok(Value::Null)
}


pub const ADAPTER: Adapter = Adapter {
    name: "Codex",
    server_name: "codex-acp",
    server: || {
        if let Some(program) = crate::platform::find_executable("codex-acp") {
            Ok((program, Vec::new()))
        } else {
            super::subagent_server(Agent::Codex)
        }
    },
    reopen: Reopen::Load,
    delete_model_session: false,
    compact_command: Some("compact"),
    default_models: || super::default_model_entry(super::Harness::Codex),
    steers: false,
    prepare: |input, plan| {
        if let Some(edits) = input.child_edits {
            let request = super::child_request(Agent::Codex, input, edits);
            plan.env.extend(super::runner::codex_env(&request).into_iter().map(|(key, value)| (key.into(), value)));
        }
        Ok(())
    },
    configure_mcp,
    launch_args: |model, _| model.map_or_else(Vec::new, |model| vec!["-c".into(), format!("model={model:?}")]),
    modes: [Some("read-only"), Some("agent"), Some("agent-full-access")],
    mode_aliases: &[("workspace-write", super::Permission::Auto)],
    full_access_relaunch: false,
    native_compaction_managed: false,
    fallback_question: false,
    discover: |_| crate::platform::find_executable("codex-acp").or_else(|| {
        let codex = crate::platform::find_executable("codex")?;
        crate::platform::find_executable("npx").map(|_| codex)
    }),
    custom_path: false,
    probe_env: super::no_probe_env,
    config_root: |home| home.join(".codex"),
    global_skills: |home| home.join(".agents/skills"),
    project_skills: ".agents/skills",
    hidden_commands: &["approvals", "logout", "resume", "fork", "mcp", "model", "permissions"],
    worktree: super::WorktreeHooks::DEFAULT,
    protocol: super::ProtocolHooks {
        async_ask: Some(decode_async_ask),
        background: Some(super::BackgroundHooks {
            from_update: background::background_update,
            finished: background::task_finished,
            live_execute_tools: true,
        }),
        ignore_update,
        ..super::ProtocolHooks::DEFAULT
    },
    sign_in: super::SignIn {
        program: "codex",
        locate: |_| super::find("codex"),
        args: &["login"],
        session_hint: "Sign in with `codex login`, then retry the session.",
        run_hint: "Codex isn't signed in. Run `codex login`, then try again.",
    },
    usage: Some(super::UsageSource {
        read: |_| Ok(vec![Pool { name: "Codex".into(), usage: limits::probe()? }]),
        every: Some(limits::REFRESH_EVERY),
        after_turn: true,
        // Rollout logs are read again only after Codex writes to them: no network and no subprocess.
        live: Some(super::LiveUsage {
            read: || limits::codex_home().and_then(|home| limits::read(&home)),
            every: std::time::Duration::from_secs(3),
        }),
    }),
    text_generation_launch,
    probe_session_meta: None,
    copies_skills: false,
    capabilities: super::capabilities::Capabilities::ALL,
    mcp_off: &[],
    reports: None,
    native_sessions: None,
    tidy: || {},
};

/// How Codex (`codex-acp`) reports background work over ACP:
/// 1. In-flight shell executions (`ExecCommandBegin` / `ExecCommandOutputDelta` / `ExecCommandEnd`) arrive as
///    `tool_call` (`kind: "execute"`, `status: "in_progress"`), streaming `tool_call_update` (`_meta.terminal_output_delta`
///    or `content`/`rawOutput`), and a final `tool_call_update` (`status: "completed" | "failed"`, `rawOutput.exit_code`
///    / `exitCode`). When a command outlives the turn, `session/prompt` returns `end_turn` while the tool is still
///    `in_progress`; `live_execute_tools: true` keeps it live until its completion update arrives.
/// 2. Unified-exec background terminals (`exec_command` / `write_stdin`) complete the initial `exec_command` tool call
///    with output like `"Process running with session ID <id>"` (or structured `rawOutput.session_id` / `process_id`).
///    Later `write_stdin` calls pass `rawInput.session_id` (integer or string) and report exit with
///    `"Process exited with code <N>"` (or a completion notice naming the session id).
mod background {
    use super::super::{BackgroundTask, BackgroundUpdate, background_output_texts, background_task_label, tool_output_texts};
    use serde_json::Value;

    const RUNNING_PREFIXES: &[&str] = &[
        "process running with session id",
        "background terminal running with session id",
    ];

    const EXIT_MARKERS: &[&str] = &[
        "process exited with code",
        "process exited",
        "stdin is closed",
        "no such session",
        "session not found",
        "process terminated",
        "process killed",
    ];

    const FINISHED_WORDS: &[&str] = &[
        "exited",
        "finished",
        "completed",
        "terminated",
        "killed",
        "canceled",
        "cancelled",
        "stopped",
    ];

    fn json_id(value: &Value) -> Option<String> {
        if let Some(s) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s.to_string())
        } else if let Some(n) = value.as_i64() {
            Some(n.to_string())
        } else {
            value.as_u64().map(|n| n.to_string())
        }
    }

    fn extract_running_session_id(text: &str) -> Option<String> {
        let lower = text.to_ascii_lowercase();
        for prefix in RUNNING_PREFIXES {
            if let Some(pos) = lower.find(prefix) {
                let rest = text[pos + prefix.len()..].trim_start_matches([':', ' ', '\t']);
                let id = rest.split_whitespace().next()?.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.' | ')' | ']'));
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
        None
    }

    fn text_indicates_exit(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        EXIT_MARKERS.iter().any(|marker| lower.contains(marker))
    }

    fn contains_task_id_token(line: &str, task_id: &str) -> Option<usize> {
        let mut start = 0;
        while let Some(rel) = line[start..].find(task_id) {
            let at = start + rel;
            let end = at + task_id.len();
            let before_ok = line[..at].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-');
            let after_ok = line[end..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-');
            if before_ok && after_ok {
                return Some(end);
            }
            start = end;
        }
        None
    }

    fn background_task_from_tool(call: &super::super::thread::ToolCall, update: &Value) -> Option<BackgroundTask> {
        let texts = tool_output_texts(call);
        let raw_out = update.get("rawOutput");
        let task_id = texts
            .iter()
            .find_map(|text| extract_running_session_id(text))
            .or_else(|| raw_out.and_then(Value::as_str).and_then(extract_running_session_id))
            .or_else(|| {
                let raw = raw_out?.as_object()?;
                let has_exit = raw.get("exit_code").is_some_and(|v| !v.is_null())
                    || raw.get("exitCode").is_some_and(|v| !v.is_null());
                if has_exit || texts.iter().any(|t| text_indicates_exit(t)) {
                    return None;
                }
                raw.get("session_id").or_else(|| raw.get("process_id")).and_then(json_id)
            })?;
        Some(BackgroundTask { id: task_id, label: background_task_label(call), log_path: None })
    }

    pub(super) fn background_update(call: &super::super::thread::ToolCall, update: &Value) -> BackgroundUpdate {
        if update.get("rawOutput").is_none() && update.get("content").is_none() {
            return BackgroundUpdate::default();
        }
        let output = background_output_texts(call, update);
        let started = (call.kind == "execute").then(|| background_task_from_tool(call, update)).flatten();
        let mut finished = Vec::new();
        if let Some(input_id) = update
            .pointer("/rawInput/session_id")
            .or_else(|| update.pointer("/rawInput/process_id"))
            .and_then(json_id)
            && (call.details.exit_code.is_some() || output.iter().any(|t| text_indicates_exit(t)))
        {
            finished.push(input_id);
        }
        BackgroundUpdate { started, finished, output }
    }

    pub(super) fn task_finished(text: &str, task_id: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        if lower.contains("no running processes")
            || lower.contains("no background terminals")
            || lower.contains("no background tasks are currently running")
            || (lower.contains("all background") && lower.contains("stopped"))
        {
            return true;
        }
        text.lines().any(|line| {
            let Some(end) = contains_task_id_token(line, task_id) else { return false };
            let rest = line[end..].trim_start_matches(['"', '\'', ')', ']', ':', ' ']).to_ascii_lowercase();
            FINISHED_WORDS.iter().any(|word| {
                rest.strip_prefix(word).is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '.', ':', ',']))
            }) || EXIT_MARKERS.iter().any(|marker| rest.contains(marker))
        })
    }
}

fn configure_mcp(tools: &crate::mcp::session::SessionTools, plan: &mut super::SessionPlan) -> Result<(), String> {
    let inherited = std::env::var(CONFIG_ENV).ok();
    let local = plan.env.iter().find(|(key, _)| key == CONFIG_ENV).map(|(_, value)| value.as_str());
    let mut config: Map<String, Value> = match inherited.filter(|text| !text.is_empty()) {
        Some(text) => serde_json::from_str(&text).map_err(|_| format!("{CONFIG_ENV} must be a JSON object."))?,
        None => Map::new(),
    };
    if let Some(local) = local {
        let local: Map<String, Value> = serde_json::from_str(local).map_err(|_| format!("{CONFIG_ENV} must be a JSON object."))?;
        config.extend(local);
    }
    let config = m4code_mcp_config(Some(&Value::Object(config).to_string()), &tools.executable, &tools.env)?;
    plan.set_env(CONFIG_ENV, config);
    Ok(())
}

/// How this agent runs as a custom subagent (`harness/subagents.rs`).
pub const PROVIDER: Provider = Provider {
    dashboard_url: None,
    color: |t| t.codex,
    marks: logos::codex_marks,
    name: "Codex",
    key: "codex",
    task_name: "OpenAI Codex",
    triggers: "Codex, GPT",
    program: "npx",
    // Pinned so an upstream adapter change reaches users only with a m4code release.
    args: &["-y", "@agentclientprotocol/codex-acp@2.6.0"],
    windows_fallback: None,
    server: "codex-acp",
    missing: "npx was not found on PATH. Codex subagents need Node.js.",
    warning: None,
    cross_harness_default: true,
    task_args: super::runner::no_args,
    environment: super::runner::codex_env,
    config_options: super::runner::no_options,
    inbox_without_steering: true,
    steering_prompt_required: true,
    deferred_tool_target: false,
    resumes_after_background: false,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn m4code_mcp_is_required_and_preserves_other_codex_config() {
        let config = m4code_mcp_config(
            Some(r#"{"model":"gpt-6","mcp_servers":{"other":{"command":"other"},"m4code":{"enabled":false}}}"#),
            Path::new("/apps/m4code"),
            &[("M4CODE_RECORD", "session-1".into()), ("M4CODE_TOOLS_OFF", "ask_user".into())],
        ).unwrap();
        let config: Value = serde_json::from_str(&config).unwrap();
        assert_eq!(config, json!({
            "model": "gpt-6",
            "mcp_servers": {
                "other": {"command": "other"},
                "m4code": {
                    "command": "/apps/m4code",
                    "args": ["--mcp-title"],
                    "env": {"M4CODE_RECORD": "session-1", "M4CODE_TOOLS_OFF": "ask_user"},
                    "required": true
                }
            }
        }));
    }

    #[test]
    fn codex_background_hooks_parse_unified_exec_start_and_finish() {
        use crate::harness::thread::{ToolCall, ToolDetails};

        let mut call = ToolCall {
            id: "exec-1".into(),
            title: "Run tests".into(),
            kind: "execute".into(),
            details: Box::new(ToolDetails {
                command: Some("cargo test --locked".into()),
                output: Some("Wall time: 10.0s\nProcess running with session ID 42\nOutput:\nrunning...".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let update = json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "exec-1",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "cmd": "cargo test --locked" },
            "rawOutput": "Wall time: 10.0s\nProcess running with session ID 42\nOutput:\nrunning..."
        });
        let bg = background::background_update(&call, &update);
        let started = bg.started.expect("unified_exec session should be detected");
        assert_eq!(started.id, "42");
        assert_eq!(started.label, "cargo test --locked");
        assert!(bg.finished.is_empty());

        // Polling via write_stdin that reports exit retires the session by rawInput.session_id.
        call.details.output = Some("Process exited with code 0\nOutput:\nall passed".into());
        let exit_update = json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "stdin-1",
            "kind": "execute",
            "status": "completed",
            "rawInput": { "session_id": 42, "chars": "" },
            "rawOutput": "Process exited with code 0\nOutput:\nall passed"
        });
        let bg = background::background_update(&call, &exit_update);
        assert!(bg.started.is_none());
        assert_eq!(bg.finished, vec!["42".to_string()]);

        // Line-based completion matching respects token boundaries and global clear phrases.
        assert!(background::task_finished("Session 42 exited with code 0.", "42"));
        assert!(!background::task_finished("Session 420 exited with code 0.", "42"));
        assert!(background::task_finished("No background terminals are running.", "42"));
    }
}
