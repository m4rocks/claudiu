//! What a harness session has said so far, built from its ACP `session/update` notifications. No GPUI here; the
//! thread view draws it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One block of the conversation, in the order it arrived.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Entry {
    User(String),
    Agent(String),
    Thought(String),
    Tool(ToolCall),
    /// Something m4code says about the session itself: a cancelled turn, a failed prompt.
    Notice { text: String, error: bool },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Ended,
}

impl ToolStatus {
    fn parse(status: &str) -> Option<Self> {
        Some(match status {
            "pending" => Self::Pending,
            "in_progress" => Self::Running,
            "completed" => Self::Done,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolContent {
    Text(String),
    Image { mime: String, data: String },
    /// A file the tool changed: its text before (none for a new file) and after.
    Diff { path: PathBuf, old: Option<String>, new: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    /// ACP's tool kind: read, edit, execute, search, fetch, think, other.
    pub kind: String,
    pub status: ToolStatus,
    pub content: Vec<ToolContent>,
    /// The files it acts on, with a line when the agent named one.
    pub locations: Vec<(PathBuf, Option<u32>)>,
    /// The agent type, when the call runs one of the harness's own subagents (`subagent_type`).
    #[serde(default)]
    pub subagent: Option<String>,
    #[serde(default, skip_serializing_if = "ToolDetails::is_empty")]
    pub details: Box<ToolDetails>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolDetails {
    pub command: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub output: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub exit_code: Option<i64>,
    pub end_line: Option<u32>,
    pub server: Option<String>,
    pub request: Option<String>,
    pub response: Option<String>,
}

impl ToolDetails {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    fn apply(&mut self, kind: &str, update: &Value) {
        let raw = &update["rawInput"];
        let out = &update["rawOutput"];
        let metadata = &update["_meta"]["m4code"];
        let set_text = |target: &mut Option<String>, value: &Value| {
            if let Some(text) = value.as_str() {
                *target = Some(text.to_string());
            }
        };
        if kind == "execute" {
            set_text(&mut self.command, &raw["command"]);
            set_text(&mut self.stdout, &out["stdout"]);
            set_text(&mut self.stderr, &out["stderr"]);
            let terminal = &update["_meta"]["terminal_output_delta"];
            if let Some(delta) = terminal["data"].as_str() {
                let stream = match terminal["stream"].as_str() {
                    Some("stdout") => &mut self.stdout,
                    Some("stderr") => &mut self.stderr,
                    _ => &mut self.output,
                };
                stream.get_or_insert_with(String::new).push_str(delta);
            } else if out["stdout"].is_string() || out["stderr"].is_string() {
                self.output = None;
            } else if let Some(output) = out.as_str().or_else(|| out["formatted_output"].as_str()).or_else(|| out["output"].as_str()) {
                self.output = Some(output.to_string());
            }
            if let Some(code) = out["exitCode"].as_i64().or_else(|| out["exit_code"].as_i64()) {
                self.exit_code = Some(code);
            }
        }
        if let Some(ms) = metadata["elapsedMs"].as_u64() {
            self.elapsed_ms = Some(ms);
        }
        if let Some(line) = metadata["endLine"].as_u64().and_then(|line| u32::try_from(line).ok()) {
            self.end_line = Some(line);
        }
        set_text(&mut self.server, &metadata["mcpServer"]);
        if let Some(delta) = update["_meta"]["mcp_output_delta"]["data"].as_str() {
            self.response.get_or_insert_with(String::new).push_str(delta);
        }
        if self.server.is_some() || kind == "fetch" {
            for (slot, value) in [(&mut self.request, raw), (&mut self.response, out)] {
                if !value.is_null() {
                    *slot = serde_json::to_string_pretty(value).ok().map(|mut text| {
                        if text.len() > 65_536 {
                            text.truncate(text.floor_char_boundary(65_536));
                            text.push_str("\n[Preview truncated]");
                        }
                        text
                    });
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanItem {
    pub text: String,
    /// pending, in_progress or completed.
    pub status: String,
}

/// A slash command the agent offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub description: String,
}

/// A setting the agent lets the client change (`session/set_config_option`), such as the model or the effort.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    /// What it is for, by ACP's categories: model, thought_level, mode, speed, or something else.
    pub category: String,
    pub current: String,
    /// `(value, name)` of each choice.
    pub choices: Vec<(String, String)>,
}

/// Token and prompt-cache counts reported for one completed turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    /// Total prompt input tokens (including cached reads and writes).
    pub input_tokens: u64,
    pub cached_read_tokens: u64,
    pub cached_write_tokens: u64,
    pub output_tokens: u64,
}

impl TurnUsage {
    /// Read token and cache counts from an ACP `PromptResponse.usage` object (or Anthropic snake_case fields).
    pub fn parse(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let num = |keys: &[&str]| keys.iter().find_map(|k| obj.get(*k).and_then(Value::as_u64));
        let cached_read = num(&["cachedReadTokens", "cacheReadTokens", "cached_read_tokens", "cache_read_input_tokens"]).unwrap_or(0);
        let cached_write = num(&["cachedWriteTokens", "cacheWriteTokens", "cached_write_tokens", "cache_creation_input_tokens"]).unwrap_or(0);
        let output_tokens = num(&["outputTokens", "output_tokens"]).unwrap_or(0);
        let input_tokens = if let Some(total_input) = num(&["inputTokens"]) {
            total_input.max(cached_read.saturating_add(cached_write))
        } else if let Some(raw_input) = num(&["input_tokens"]) {
            // Anthropic's snake_case `input_tokens` excludes cache reads and writes.
            raw_input.saturating_add(cached_read).saturating_add(cached_write)
        } else {
            cached_read.saturating_add(cached_write)
        };
        if input_tokens == 0 && output_tokens == 0 && cached_read == 0 && cached_write == 0 {
            return None;
        }
        Some(Self { input_tokens, cached_read_tokens: cached_read, cached_write_tokens: cached_write, output_tokens })
    }
}

/// Cumulative and last-turn prompt-cache measurements for a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheUsage {
    pub turns: u64,
    pub input_tokens: u64,
    pub cached_read_tokens: u64,
    pub cached_write_tokens: u64,
    pub output_tokens: u64,
    pub last_input_tokens: u64,
    pub last_cached_read_tokens: u64,
    pub last_cached_write_tokens: u64,
}

impl CacheUsage {
    pub fn record(&mut self, turn: TurnUsage) {
        self.turns = self.turns.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(turn.input_tokens);
        self.cached_read_tokens = self.cached_read_tokens.saturating_add(turn.cached_read_tokens);
        self.cached_write_tokens = self.cached_write_tokens.saturating_add(turn.cached_write_tokens);
        self.output_tokens = self.output_tokens.saturating_add(turn.output_tokens);
        self.last_input_tokens = turn.input_tokens;
        self.last_cached_read_tokens = turn.cached_read_tokens;
        self.last_cached_write_tokens = turn.cached_write_tokens;
    }

    /// Fraction of all measured input tokens served from prompt cache (0.0..=1.0).
    pub fn hit_rate(&self) -> Option<f32> {
        (self.input_tokens > 0).then(|| (self.cached_read_tokens as f64 / self.input_tokens as f64).clamp(0.0, 1.0) as f32)
    }

    /// Fraction of the most recent turn's input tokens served from prompt cache (0.0..=1.0).
    #[cfg(test)]
    pub fn last_hit_rate(&self) -> Option<f32> {
        (self.last_input_tokens > 0).then(|| (self.last_cached_read_tokens as f64 / self.last_input_tokens as f64).clamp(0.0, 1.0) as f32)
    }

    /// Short human-readable summary for the inspector, e.g. `"84% hit · 120k read"`.
    pub fn summary(&self) -> Option<String> {
        let rate = self.hit_rate()?;
        let pct = (rate * 100.0).round() as u64;
        let read = format_tokens(self.cached_read_tokens);
        Some(format!("{pct}% hit · {read} read"))
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thread {
    pub entries: Vec<Entry>,
    /// The agent's latest plan; each update replaces the whole list.
    pub plan: Vec<PlanItem>,
    pub commands: Vec<Command>,
    pub title: Option<String>,
    /// The permission modes, as `(id, name)`, and the one in use.
    pub modes: Vec<(String, String)>,
    pub mode: Option<String>,
    pub config: Vec<ConfigOption>,
    /// Tokens of context in use, and how many fit.
    pub context: Option<(u64, u64)>,
    /// Measured prompt-cache usage across turns in this view.
    pub cache_usage: CacheUsage,
}

impl Thread {
    /// Take in one `session/update`'s `update` object, and say which entry changed first, if any did; the view
    /// measures that entry and those after it again. Kinds this client didn't ask for are skipped.
    pub fn apply(&mut self, update: &Value) -> Option<usize> {
        let text = || update.pointer("/content/text").and_then(Value::as_str).unwrap_or_default();
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "user_message_chunk" => {
                let at = self.append(text(), Entry::User, |e| match e {
                    Entry::User(t) => Some(t),
                    _ => None,
                });
                // A replayed hand-off arrives in pieces, so the whole message so far is checked each time.
                if let Some(Entry::User(t)) = self.entries.last_mut()
                    && let Some(typed) = typed_part(t)
                {
                    if typed.is_empty() {
                        // An agent's own hand-off: nothing the user typed.
                        self.entries.pop();
                        return None;
                    }
                    *t = typed.to_string();
                }
                return at;
            }
            "agent_message_chunk" => return self.append(text(), Entry::Agent, |e| match e {
                Entry::Agent(t) => Some(t),
                _ => None,
            }),
            "agent_thought_chunk" => return self.append(text(), Entry::Thought, |e| match e {
                Entry::Thought(t) => Some(t),
                _ => None,
            }),
            "tool_call" => {
                let id = update["toolCallId"].as_str().unwrap_or_default();
                // A repeated `tool_call` for an id it already sent updates that call, as the protocol allows.
                if self.tool_at(id).is_none() {
                    self.entries.push(Entry::Tool(ToolCall { id: id.to_string(), ..Default::default() }));
                }
                return self.update_tool(id, update);
            }
            "tool_call_update" => return self.update_tool(update["toolCallId"].as_str().unwrap_or_default(), update),
            "plan" => {
                self.plan = items(update, "entries")
                    .map(|e| PlanItem { text: str_of(e, "content"), status: str_of(e, "status") })
                    .collect();
            }
            "available_commands_update" => {
                self.commands = items(update, "availableCommands")
                    .map(|c| Command { name: str_of(c, "name"), description: str_of(c, "description") })
                    .collect();
            }
            "current_mode_update" => self.mode = update["currentModeId"].as_str().map(String::from),
            "config_option_update" => self.set_config(&update["configOptions"]),
            "session_info_update" => {
                if let Some(title) = update["title"].as_str().filter(|t| !t.is_empty()) {
                    self.title = Some(title.to_string());
                }
            }
            "usage_update" => {
                if let (Some(used), Some(size)) = (update["used"].as_u64(), update["size"].as_u64()) {
                    self.context = Some((used, size));
                }
            }
            _ => {}
        }
        None
    }

    /// Take in what `session/new` or `session/load` answered: the modes and the config options.
    pub fn apply_session(&mut self, result: &Value) {
        if let Some(modes) = result.get("modes").filter(|m| m.is_object()) {
            self.modes = items(modes, "availableModes").map(|m| (str_of(m, "id"), str_of(m, "name"))).collect();
            self.mode = modes["currentModeId"].as_str().map(String::from);
        }
        if result.get("configOptions").is_some() {
            self.set_config(&result["configOptions"]);
        }
    }

    fn set_config(&mut self, options: &Value) {
        self.config = options
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| ConfigOption {
                id: str_of(o, "id"),
                name: str_of(o, "name"),
                category: str_of(o, "category"),
                current: str_of(o, "currentValue"),
                choices: items(o, "options").map(|c| (str_of(c, "value"), str_of(c, "name"))).collect(),
            })
            .collect();
        // Claude's adapter lists the permission mode among its options too; the mode cycle reads it from either.
        if let Some(mode) = self.config.iter().find(|o| o.category == "mode") {
            if self.modes.is_empty() {
                self.modes = mode.choices.clone();
            }
            self.mode = Some(mode.current.clone());
        }
    }

    pub fn option(&self, category: &str) -> Option<&ConfigOption> {
        self.config.iter().find(|o| o.category == category)
    }

    /// Record token/cache usage from a completed turn's `usage` payload.
    pub fn record_usage(&mut self, usage: &Value) -> Option<TurnUsage> {
        let turn = TurnUsage::parse(usage)?;
        self.cache_usage.record(turn);
        Some(turn)
    }

    /// Add something m4code says, and return where it went.
    pub fn notice(&mut self, text: impl Into<String>, error: bool) -> usize {
        self.entries.push(Entry::Notice { text: text.into(), error });
        self.entries.len() - 1
    }

    /// Add what the user sent, and return where it went. Agents don't echo a live prompt back.
    pub fn sent(&mut self, text: impl Into<String>) -> Option<usize> {
        let text = text.into();
        let shown = typed_part(&text).map_or(text.clone(), String::from);
        if shown.is_empty() {
            return None;
        }
        self.entries.push(Entry::User(shown));
        Some(self.entries.len() - 1)
    }

    /// What the user sent, oldest first: the composer's history.
    pub fn sent_texts(&self) -> Vec<String> {
        self.entries.iter().filter_map(|e| match e {
            Entry::User(t) if parse_subagent_report(t).is_none() => Some(t.clone()),
            _ => None,
        }).collect()
    }

    #[cfg(test)]
    pub fn tool(&self, id: &str) -> Option<&ToolCall> {
        self.entries.iter().find_map(|e| match e {
            Entry::Tool(call) if call.id == id => Some(call),
            _ => None,
        })
    }

    pub fn tool_at(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|e| matches!(e, Entry::Tool(call) if call.id == id))
    }

    fn update_tool(&mut self, id: &str, update: &Value) -> Option<usize> {
        let at = self.tool_at(id)?;
        if let Entry::Tool(call) = &mut self.entries[at] {
            merge_tool(call, update);
        }
        Some(at)
    }

    /// Add a chunk to the last entry when it is of the same kind, or start a new one.
    fn append(&mut self, chunk: &str, make: fn(String) -> Entry, same: fn(&mut Entry) -> Option<&mut String>) -> Option<usize> {
        if chunk.is_empty() {
            return None;
        }
        match self.entries.last_mut().and_then(same) {
            Some(text) => text.push_str(chunk),
            None => self.entries.push(make(chunk.to_string())),
        }
        Some(self.entries.len() - 1)
    }

    /// The conversation as plain text, newest last, for another session to read or for a hand-off summary.
    pub fn transcript(&self, last: usize) -> String {
        self.transcript_after(0, last)
    }

    pub fn transcript_after(&self, after: usize, last: usize) -> String {
        let after = if after <= self.entries.len() { after } else { 0 };
        let start = self.entries.len().saturating_sub(last).max(after);
        self.entries[start..]
            .iter()
            .filter_map(|e| match e {
                Entry::User(t) => Some(format!("User: {t}")),
                Entry::Agent(t) => Some(format!("Agent: {t}")),
                Entry::Tool(call) => Some(format!("[{}: {}]", call.kind, call.title)),
                Entry::Thought(_) | Entry::Notice { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Only the fields an update carries change; the rest of the call stays as it was.
/// How much of the conversation a hand-off carries, newest kept: enough for the plan and recent steps, without
/// filling the new agent's context.
const HANDOFF_MAX_CHARS: usize = 40_000;
const HANDOFF_OPENING: &str = "You are taking over work from a ";
const HANDOFF_NEXT: &str = "\n\nUser's next message:\n";

/// The first message a hand-off sends the new agent: `transcript` of the conversation so far, then `user_message`,
/// empty when the agent handed over on its own. The chat shows only `user_message` (see `typed_part`).
pub fn handoff_prompt(from: &str, transcript: &str, user_message: &str) -> String {
    let mut start = transcript.len().saturating_sub(HANDOFF_MAX_CHARS);
    while !transcript.is_char_boundary(start) {
        start += 1;
    }
    let cut = if start > 0 { "[earlier messages left out]\n\n" } else { "" };
    format!(
        "{HANDOFF_OPENING}{from} session in this folder. Its conversation so far is below; tool steps show as \
         [kind: title]. Read it, check the current state of the files, then {next}.\n\n\
         <conversation>\n{cut}{}\n</conversation>{HANDOFF_NEXT}{user_message}",
        &transcript[start..],
        next = if user_message.is_empty() { "carry on where the last agent message leaves off" } else { "answer the user's next message" },
    )
}

/// What the user typed, from a message that may be a hand-off prompt: the transcript in front of it was already on
/// screen, and copying it into the chat would also nest it into the next hand-off.
fn typed_part(text: &str) -> Option<&str> {
    if !text.starts_with(HANDOFF_OPENING) {
        return None;
    }
    text.find(HANDOFF_NEXT).map(|at| &text[at + HANDOFF_NEXT.len()..])
}

const REPORT_OPENING: &str = "[m4code subagent report]\n";
const REPORT_BODY: &str = "\n\n";

/// What a subagent said when its turn ended, as m4code sends it to the parent session (`subagent_report_prompt`).
#[derive(Debug, PartialEq)]
pub struct SubagentReport<'a> {
    pub id: &'a str,
    /// The child's harness key, such as `codex`.
    pub harness: &'a str,
    pub title: &'a str,
    /// `completed`, `failed` or `interrupted`.
    pub state: &'a str,
    pub body: &'a str,
}

/// The message that hands a subagent's report to its parent. The header lines are for both the model and
/// `parse_subagent_report`, which lets the parent's chat draw it as a report card instead of a user message.
pub fn subagent_report_prompt(id: &str, harness: &str, title: &str, state: &str, body: &str) -> String {
    let title = title.replace(['\n', '\r'], " ");
    format!(
        "{REPORT_OPENING}id: {id}\nharness: {harness}\ntitle: {title}\nstate: {state}\n\
         note: Your subagent's turn ended. Continue your task with its report below; send_subagent_message reaches it again.\
         {REPORT_BODY}{}",
        if body.trim().is_empty() { "(The subagent wrote no report.)" } else { body }
    )
}

/// A parent's chat message that is a subagent's report, read back from `subagent_report_prompt`'s text.
pub fn parse_subagent_report(text: &str) -> Option<SubagentReport<'_>> {
    let rest = text.strip_prefix(REPORT_OPENING)?;
    let (header, body) = rest.split_once(REPORT_BODY)?;
    let field = |key: &str| header.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix(": "));
    Some(SubagentReport { id: field("id")?, harness: field("harness")?, title: field("title")?, state: field("state")?, body })
}

fn merge_tool(call: &mut ToolCall, update: &Value) {
    if let Some(title) = update["title"].as_str() {
        call.title = title.to_string();
    }
    if let Some(kind) = update["kind"].as_str() {
        call.kind = kind.to_string();
    }
    if let Some(status) = update["status"].as_str().and_then(ToolStatus::parse) {
        call.status = status;
    }
    if update.get("content").is_some_and(Value::is_array) {
        call.content = items(update, "content").filter_map(tool_content).collect();
    }
    if update.get("locations").is_some_and(Value::is_array) {
        call.locations = items(update, "locations")
            .filter_map(|l| Some((PathBuf::from(l["path"].as_str()?), l["line"].as_u64().map(|n| n as u32))))
            .collect();
    }
    call.details.apply(&call.kind, update);
    if let Some(agent_type) = subagent_type(update) {
        call.subagent = Some(agent_type);
    }
}

/// The agent type a tool call starts, when it starts one of the harness's own subagents: Claude Code's Agent tool and
/// OpenCode's task tool name it in `subagent_type`. Claude Code leaves it out for its general-purpose agent; m4code's
/// bridge gives only that tool the `think` kind together with a description and a prompt.
fn subagent_type(update: &Value) -> Option<String> {
    let input = &update["rawInput"];
    if let Some(agent_type) = input["subagent_type"].as_str().filter(|t| !t.is_empty()) {
        return Some(agent_type.to_string());
    }
    (update["kind"] == "think" && input["description"].is_string() && input["prompt"].is_string()).then(|| "general-purpose".to_string())
}

fn tool_content(item: &Value) -> Option<ToolContent> {
    match item["type"].as_str()? {
        "content" => {
            let content = &item["content"];
            if content["type"] == "image" {
                let mime = content["mimeType"].as_str()?;
                let data = content["data"].as_str()?;
                (matches!(mime, "image/png" | "image/jpeg" | "image/webp") && data.len() <= 8 * 1024 * 1024)
                    .then(|| ToolContent::Image { mime: mime.into(), data: data.into() })
            } else {
                content["text"].as_str().map(|text| ToolContent::Text(text.to_string()))
            }
        }
        "diff" => Some(ToolContent::Diff {
            path: PathBuf::from(item["path"].as_str()?),
            old: item["oldText"].as_str().map(String::from),
            new: item["newText"].as_str().unwrap_or_default().to_string(),
        }),
        _ => None,
    }
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

fn str_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

/// Where m4code persists a session's conversation entries so harnesses that resume without replaying (such as
/// Antigravity) and sessions reconnecting after a restart never open blank.
fn entries_path(record_id: &str) -> PathBuf {
    crate::datadir::data_dir().join("threads").join(format!("{}.json", crate::datadir::safe_file_stem(record_id)))
}

pub fn save_entries(record_id: &str, entries: &[Entry]) {
    if record_id.is_empty() || entries.is_empty() {
        return;
    }
    if let Ok(json) = serde_json::to_string(entries) {
        let _ = crate::datadir::write_atomic(&entries_path(record_id), &json);
    }
}

pub fn load_entries(record_id: &str) -> Vec<Entry> {
    if record_id.is_empty() {
        return Vec::new();
    }
    std::fs::read_to_string(entries_path(record_id))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread(updates: &[Value]) -> Thread {
        let mut t = Thread::default();
        for u in updates {
            t.apply(u);
        }
        t
    }

    fn chunk(kind: &str, text: &str) -> Value {
        json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
    }

    #[test]
    fn a_subagent_report_reads_back_and_other_messages_do_not() {
        let text = subagent_report_prompt("c1", "codex", "Fix\nlogin", "completed", "Done.\n\nTwo files changed.");
        assert_eq!(
            parse_subagent_report(&text),
            Some(SubagentReport { id: "c1", harness: "codex", title: "Fix login", state: "completed", body: "Done.\n\nTwo files changed." })
        );
        let empty = subagent_report_prompt("c2", "claude", "t", "failed", "  ");
        assert_eq!(parse_subagent_report(&empty).unwrap().body, "(The subagent wrote no report.)");
        assert_eq!(parse_subagent_report("id: c1\nharness: codex"), None, "a user's own text is not a report");
        assert_eq!(parse_subagent_report("[m4code subagent report]\nid: c1\n\nbody"), None, "missing fields are not a report");
    }

    #[test]
    fn a_handoff_carries_the_conversation_and_the_new_message_keeping_the_end_when_long() {
        let short = handoff_prompt("Claude Code", "User: plan it\n\nAgent: step 1", "now implement step 2");
        assert!(short.contains("from a Claude Code session") && short.contains("<conversation>\nUser: plan it\n\nAgent: step 1\n</conversation>"));
        assert!(short.ends_with("User's next message:\nnow implement step 2"));
        assert!(!short.contains("left out"));

        let long = format!("User: {}é the end", "x".repeat(HANDOFF_MAX_CHARS));
        let message = handoff_prompt("Codex", &long, "finish up");
        assert!(message.contains("[earlier messages left out]") && message.contains("é the end\n</conversation>"), "the newest part goes along");
        assert!(message.ends_with("User's next message:\nfinish up"));
        assert!(!message.contains("User: x"), "the oldest part is cut");
    }

    #[test]
    fn a_hand_off_shows_only_the_message_the_user_typed() {
        let prompt = handoff_prompt("Claude Code", "User: plan it\n\nAgent: step 1", "now implement step 2");
        let mut live = Thread::default();
        live.sent(prompt.clone());
        assert_eq!(live.entries, [Entry::User("now implement step 2".into())]);

        // Replayed after a restart, in pieces that split the wording.
        let (head, tail) = prompt.split_at(prompt.len() - 25);
        let replayed = thread(&[chunk("user_message_chunk", head), chunk("user_message_chunk", tail)]);
        assert_eq!(replayed.entries, [Entry::User("now implement step 2".into())]);

        // An agent's own hand-off has no message from the user, so the chat gains none.
        let by_agent = handoff_prompt("Claude Code", "User: plan it\n\nAgent: Carry on with step 2", "");
        let mut live = Thread::default();
        live.sent(by_agent.clone());
        assert!(live.entries.is_empty());
        assert!(thread(&[chunk("user_message_chunk", &by_agent)]).entries.is_empty());

        let quoted = "What does \"User's next message:\" mean?";
        assert_eq!(thread(&[chunk("user_message_chunk", quoted)]).entries, [Entry::User(quoted.into())], "an ordinary message stays whole");
    }

    #[test]
    fn chunks_of_one_kind_join_and_a_tool_call_starts_a_new_message() {
        let t = thread(&[
            chunk("user_message_chunk", "make a file"),
            chunk("agent_thought_chunk", "thinking"),
            chunk("agent_message_chunk", "On "),
            chunk("agent_message_chunk", "it."),
            json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Terminal", "kind": "execute", "status": "pending"}),
            chunk("agent_message_chunk", "Done."),
        ]);
        assert_eq!(t.entries.len(), 5);
        assert_eq!(t.entries[0], Entry::User("make a file".into()));
        assert_eq!(t.entries[1], Entry::Thought("thinking".into()));
        assert_eq!(t.entries[2], Entry::Agent("On it.".into()));
        assert_eq!(t.entries[4], Entry::Agent("Done.".into()));
    }

    #[test]
    fn a_call_that_starts_a_subagent_names_its_type_and_others_do_not() {
        let t = thread(&[
            json!({"sessionUpdate": "tool_call", "toolCallId": "a", "title": "Review", "kind": "think", "rawInput": {"description": "Review", "prompt": "go", "subagent_type": "m4code:codex-gpt-6"}}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "b", "title": "Look", "kind": "think", "rawInput": {"description": "Look", "prompt": "go"}}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "c", "title": "Read", "kind": "read", "rawInput": {"file_path": "a.rs"}}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "d", "title": "Plan", "kind": "think", "rawInput": {"description": "only a description"}}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "a", "status": "completed"}),
        ]);
        let types: Vec<Option<&str>> = t.entries.iter().map(|e| match e { Entry::Tool(c) => c.subagent.as_deref(), _ => None }).collect();
        assert_eq!(types, [Some("m4code:codex-gpt-6"), Some("general-purpose"), None, None]);
    }

    #[test]
    fn a_tool_update_changes_only_what_it_carries() {
        let t = thread(&[
            json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Terminal", "kind": "execute", "status": "pending", "content": []}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "title": "printf hi > a.txt"}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "hi"}}]}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "unknown", "status": "failed"}),
        ]);
        let call = t.tool("t1").unwrap();
        assert_eq!(call.title, "printf hi > a.txt");
        assert_eq!(call.kind, "execute", "kept from the first message");
        assert_eq!(call.status, ToolStatus::Done);
        assert_eq!(call.content, vec![ToolContent::Text("hi".into())]);
        assert_eq!(t.entries.len(), 1, "an update for a call never announced adds nothing");
    }

    #[test]
    fn shell_deltas_accumulate_and_final_output_replaces_the_snapshot() {
        let state = thread(&[
            json!({"sessionUpdate":"tool_call", "toolCallId":"shell", "kind":"execute", "rawInput":{"command":"npm test"}}),
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"shell", "_meta":{"terminal_output_delta":{"data":"first\n"}}}),
            json!({"sessionUpdate":"tool_call_update", "toolCallId":"shell", "_meta":{"terminal_output_delta":{"data":"second\n"}}}),
        ]);
        assert_eq!(state.tool("shell").unwrap().details.output.as_deref(), Some("first\nsecond\n"));
        let mut state = state;
        state.apply(&json!({"sessionUpdate":"tool_call_update", "toolCallId":"shell", "status":"failed",
            "rawOutput":{"formatted_output":"first\nsecond\nfailed\n","exit_code":1}}));
        let tool = state.tool("shell").unwrap();
        assert_eq!(tool.details.command.as_deref(), Some("npm test"));
        assert_eq!(tool.details.output.as_deref(), Some("first\nsecond\nfailed\n"));
        assert_eq!(tool.details.exit_code, Some(1));
    }

    #[test]
    fn each_update_names_the_entry_it_changed() {
        let mut t = Thread::default();
        assert_eq!(t.apply(&chunk("agent_message_chunk", "a")), Some(0));
        assert_eq!(t.apply(&json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "status": "pending"})), Some(1));
        assert_eq!(t.apply(&chunk("agent_message_chunk", "b")), Some(2));
        assert_eq!(t.apply(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"})), Some(1), "an earlier call");
        assert_eq!(t.apply(&chunk("agent_message_chunk", "")), None, "an empty chunk changes nothing");
        assert_eq!(t.apply(&json!({"sessionUpdate": "usage_update", "used": 1, "size": 2})), None, "not an entry");
        assert_eq!(t.sent("next"), Some(3));
        assert_eq!(t.sent_texts(), vec!["next".to_string()]);
    }

    #[test]
    fn a_diff_and_its_locations_are_read() {
        let t = thread(&[json!({
            "sessionUpdate": "tool_call", "toolCallId": "e", "title": "Edit a.rs", "kind": "edit", "status": "completed",
            "content": [{"type": "diff", "path": "/w/a.rs", "oldText": "a", "newText": "b"}, {"type": "diff", "path": "/w/new.rs", "newText": "n"}],
            "locations": [{"path": "/w/a.rs", "line": 3}, {"path": "/w/new.rs"}],
        })]);
        let call = t.tool("e").unwrap();
        assert_eq!(call.content[0], ToolContent::Diff { path: "/w/a.rs".into(), old: Some("a".into()), new: "b".into() });
        assert_eq!(call.content[1], ToolContent::Diff { path: "/w/new.rs".into(), old: None, new: "n".into() });
        assert_eq!(call.locations, vec![(PathBuf::from("/w/a.rs"), Some(3)), (PathBuf::from("/w/new.rs"), None)]);
    }

    #[test]
    fn the_mode_follows_the_config_option() {
        let mut t = Thread::default();
        t.apply_session(&json!({
            "modes": {"currentModeId": "auto", "availableModes": [{"id": "default", "name": "Manual"}, {"id": "plan", "name": "Plan"}, {"id": "auto", "name": "Auto"}]},
        }));
        assert_eq!(t.mode.as_deref(), Some("auto"));
        t.apply(&json!({"sessionUpdate": "config_option_update", "configOptions": [
            {"id": "mode", "name": "Mode", "category": "mode", "currentValue": "plan", "options": [{"value": "default", "name": "Manual"}]},
        ]}));
        assert_eq!(t.mode.as_deref(), Some("plan"));
        assert_eq!(t.modes.len(), 3, "the modes the session listed stay");
    }

    #[test]
    fn title_usage_plan_and_commands_are_kept() {
        let t = thread(&[
            json!({"sessionUpdate": "session_info_update", "title": "Hello file"}),
            json!({"sessionUpdate": "session_info_update", "title": ""}),
            json!({"sessionUpdate": "usage_update", "used": 15318, "size": 400000}),
            json!({"sessionUpdate": "plan", "entries": [{"content": "Write it", "status": "in_progress", "priority": "high"}]}),
            json!({"sessionUpdate": "available_commands_update", "availableCommands": [{"name": "review", "description": "Review a PR"}]}),
        ]);
        assert_eq!(t.title.as_deref(), Some("Hello file"), "an empty title leaves the last one");
        assert_eq!(t.context, Some((15318, 400000)));
        assert_eq!(t.plan, vec![PlanItem { text: "Write it".into(), status: "in_progress".into() }]);
        assert_eq!(t.commands, vec![Command { name: "review".into(), description: "Review a PR".into() }]);
    }

    #[test]
    fn entries_roundtrip_on_disk() {
        let _dirs = crate::datadir::use_throwaway_folders();
        let entries = vec![
            Entry::User("hello".into()),
            Entry::Thought("thinking".into()),
            Entry::Tool(ToolCall {
                id: "t1".into(),
                title: "Read a.rs".into(),
                kind: "read".into(),
                status: ToolStatus::Done,
                content: vec![ToolContent::Text("ok".into())],
                details: Default::default(),
                locations: vec![(PathBuf::from("a.rs"), Some(10))],
                subagent: Some("general-purpose".into()),
            }),
            Entry::Agent("hi there".into()),
        ];
        save_entries("test-roundtrip-session", &entries);
        assert_eq!(load_entries("test-roundtrip-session"), entries);
    }

    #[test]
    fn the_transcript_leaves_out_thoughts_and_notices() {
        let mut t = thread(&[chunk("user_message_chunk", "hi"), chunk("agent_thought_chunk", "hm"), chunk("agent_message_chunk", "hello")]);
        t.notice("Interrupted", false);
        assert_eq!(t.transcript(10), "User: hi\n\nAgent: hello");
        assert_eq!(t.transcript(2), "Agent: hello", "only the last entries");
    }

    #[test]
    fn cache_usage_parses_acp_and_anthropic_shapes_and_computes_hit_rates() {
        let acp = TurnUsage::parse(&json!({
            "inputTokens": 10_000,
            "cachedReadTokens": 8_000,
            "cachedWriteTokens": 500,
            "outputTokens": 250,
        }))
        .unwrap();
        assert_eq!(acp, TurnUsage { input_tokens: 10_000, cached_read_tokens: 8_000, cached_write_tokens: 500, output_tokens: 250 });

        let anthropic = TurnUsage::parse(&json!({
            "input_tokens": 1_000,
            "cache_read_input_tokens": 7_000,
            "cache_creation_input_tokens": 2_000,
            "output_tokens": 300,
        }))
        .unwrap();
        assert_eq!(anthropic, TurnUsage { input_tokens: 10_000, cached_read_tokens: 7_000, cached_write_tokens: 2_000, output_tokens: 300 });

        assert_eq!(TurnUsage::parse(&json!({})), None);
        assert_eq!(TurnUsage::parse(&Value::Null), None);

        let mut usage = CacheUsage::default();
        assert_eq!(usage.summary(), None);
        usage.record(acp);
        assert!((usage.hit_rate().unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(usage.summary().as_deref(), Some("80% hit · 8k read"));
        usage.record(anthropic);
        assert_eq!(usage.turns, 2);
        assert_eq!(usage.input_tokens, 20_000);
        assert_eq!(usage.cached_read_tokens, 15_000);
        assert!((usage.hit_rate().unwrap() - 0.75).abs() < 1e-6);
        assert!((usage.last_hit_rate().unwrap() - 0.7).abs() < 1e-6);
        assert_eq!(usage.summary().as_deref(), Some("75% hit · 15k read"));
    }
}
