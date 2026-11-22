//! What a harness session has said so far, built from its ACP `session/update` notifications. No GPUI here; the
//! thread view draws it.

use std::path::PathBuf;

use serde_json::Value;

/// One block of the conversation, in the order it arrived.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    User(String),
    Agent(String),
    Thought(String),
    Tool(ToolCall),
    /// Something Claudiu says about the session itself: a cancelled turn, a failed prompt.
    Notice { text: String, error: bool },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
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

#[derive(Clone, Debug, PartialEq)]
pub enum ToolContent {
    Text(String),
    /// A file the tool changed: its text before (none for a new file) and after.
    Diff { path: PathBuf, old: Option<String>, new: String },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    /// ACP's tool kind: read, edit, execute, search, fetch, think, other.
    pub kind: String,
    pub status: ToolStatus,
    pub content: Vec<ToolContent>,
    /// The files it acts on, with a line when the agent named one.
    pub locations: Vec<(PathBuf, Option<u32>)>,
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

    /// The mode after the current one, as Shift+Tab steps through them.
    pub fn next_mode(&self) -> Option<&str> {
        let at = self.modes.iter().position(|(id, _)| Some(id) == self.mode.as_ref()).map_or(0, |i| i + 1);
        self.modes.get(at % self.modes.len().max(1)).map(|(id, _)| id.as_str())
    }

    pub fn mode_name(&self) -> Option<&str> {
        self.modes.iter().find(|(id, _)| Some(id) == self.mode.as_ref()).map(|(_, name)| name.as_str())
    }

    pub fn option(&self, category: &str) -> Option<&ConfigOption> {
        self.config.iter().find(|o| o.category == category)
    }

    /// Add something Claudiu says, and return where it went.
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
            Entry::User(t) => Some(t.clone()),
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

    fn tool_at(&self, id: &str) -> Option<usize> {
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
        let start = self.entries.len().saturating_sub(last);
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
}

fn tool_content(item: &Value) -> Option<ToolContent> {
    match item["type"].as_str()? {
        "content" => item.pointer("/content/text").and_then(Value::as_str).map(|t| ToolContent::Text(t.to_string())),
        "diff" => Some(ToolContent::Diff {
            path: PathBuf::from(item["path"].as_str()?),
            old: item["oldText"].as_str().map(String::from),
            new: item["newText"].as_str().unwrap_or_default().to_string(),
        }),
        _ => None,
    }
}

/// The changed lines of a file and a few around them, each marked `+`, `-` or ` `, through git's own diff.
pub fn diff_lines(old: &str, new: &str) -> Vec<(char, String)> {
    let mut out = vec![];
    let mut options = git2::DiffOptions::new();
    options.context_lines(2);
    let Ok(patch) = git2::Patch::from_buffers(old.as_bytes(), None, new.as_bytes(), None, Some(&mut options)) else { return out };
    for hunk in 0..patch.num_hunks() {
        for line in 0..patch.num_lines_in_hunk(hunk).unwrap_or(0) {
            let Ok(l) = patch.line_in_hunk(hunk, line) else { continue };
            let sign = match l.origin() {
                '+' | '-' => l.origin(),
                _ => ' ',
            };
            out.push((sign, String::from_utf8_lossy(l.content()).trim_end_matches(['\n', '\r']).to_string()));
        }
    }
    out
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

fn str_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
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
    fn the_mode_cycle_wraps_and_follows_the_config_option() {
        let mut t = Thread::default();
        t.apply_session(&json!({
            "modes": {"currentModeId": "auto", "availableModes": [{"id": "default", "name": "Manual"}, {"id": "plan", "name": "Plan"}, {"id": "auto", "name": "Auto"}]},
        }));
        assert_eq!(t.mode_name(), Some("Auto"));
        assert_eq!(t.next_mode(), Some("default"), "after the last comes the first");
        t.apply(&json!({"sessionUpdate": "config_option_update", "configOptions": [
            {"id": "mode", "name": "Mode", "category": "mode", "currentValue": "plan", "options": [{"value": "default", "name": "Manual"}]},
        ]}));
        assert_eq!(t.mode.as_deref(), Some("plan"));
        assert_eq!(t.next_mode(), Some("auto"));
        assert_eq!(t.modes.len(), 3, "the modes the session listed stay");
    }

    #[test]
    fn an_agent_with_no_modes_has_no_next_mode() {
        assert_eq!(Thread::default().next_mode(), None);
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
    fn a_diff_marks_what_changed_with_context_around_it() {
        let old = "a\nb\nc\nd\ne\nf\ng\n";
        let new = "a\nb\nc\nD\ne\nf\ng\n";
        let lines = diff_lines(old, new);
        let marks: String = lines.iter().map(|(m, _)| *m).collect();
        assert_eq!(marks, "  -+  ");
        assert_eq!(lines[2], ('-', "d".into()));
        assert_eq!(lines[3], ('+', "D".into()));
        assert_eq!(diff_lines("", "new\n"), vec![('+', "new".into())], "a new file is all added");
    }

    #[test]
    fn the_transcript_leaves_out_thoughts_and_notices() {
        let mut t = thread(&[chunk("user_message_chunk", "hi"), chunk("agent_thought_chunk", "hm"), chunk("agent_message_chunk", "hello")]);
        t.notice("Interrupted", false);
        assert_eq!(t.transcript(10), "User: hi\n\nAgent: hello");
        assert_eq!(t.transcript(2), "Agent: hello", "only the last entries");
    }
}
