//! What a chat shows, built from the agent's `session/update` notifications. No GPUI here: `apply` is the one
//! place that knows the update shapes, so the view only draws `Item`s.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub id: String,
    pub title: String,
    pub kind: String,
    /// `pending`, `in_progress`, `completed` or `failed`.
    pub status: String,
    pub output: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    User(String),
    Agent(String),
    Thought(String),
    Tool(Tool),
    /// (entry, status) pairs.
    Plan(Vec<(String, String)>),
    Notice { text: String, error: bool },
}

/// A skill or slash command the agent offers (`available_commands_update`).
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub name: String,
    pub description: String,
    /// Present when the command takes arguments.
    pub hint: Option<String>,
}

/// A session setting the agent lets the client change (mode, model, effort…).
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub current: String,
    /// (value, label)
    pub choices: Vec<(String, String)>,
}

impl ConfigOption {
    pub fn current_label(&self) -> &str {
        self.choices.iter().find(|(v, _)| *v == self.current).map_or(self.current.as_str(), |(_, l)| l.as_str())
    }
}

#[derive(Default)]
pub struct Meta {
    pub commands: Vec<Command>,
    pub options: Vec<ConfigOption>,
    /// Context tokens used and the window size.
    pub context: Option<(u64, u64)>,
    pub title: Option<String>,
}

fn text_of(v: &Value) -> Option<&str> {
    v.get("content").and_then(|c| c.get("text")).and_then(Value::as_str)
}

pub fn parse_options(v: &Value) -> Vec<ConfigOption> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|o| o.get("type").and_then(Value::as_str) == Some("select"))
        .filter_map(|o| {
            let s = |k: &str| o.get(k).and_then(Value::as_str).map(String::from);
            let choices = o
                .get("options")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|c| Some((c.get("value")?.as_str()?.to_string(), c.get("name")?.as_str()?.to_string())))
                .collect();
            Some(ConfigOption { id: s("id")?, name: s("name")?, current: s("currentValue")?, choices })
        })
        .collect()
}

/// Text a finished tool call shows: the raw output when it is text, else the text blocks of its content.
fn tool_output(u: &Value) -> Option<String> {
    if let Some(s) = u.get("rawOutput").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    let blocks = u.get("content")?.as_array()?;
    let text: Vec<String> = blocks
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str)? {
            "content" => text_of(b).map(String::from),
            "diff" => Some(format!("changed {}", b.get("path")?.as_str()?)),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then(|| text.join("\n"))
}

/// Append `text` to the last item when `pick` finds it a same-kind block (a streamed message), else start `new`.
fn stream(items: &mut Vec<Item>, text: &str, same: fn(&mut Item) -> Option<&mut String>, new: fn(String) -> Item) {
    if let Some(s) = items.last_mut().and_then(same) {
        s.push_str(text);
    } else {
        items.push(new(text.to_string()));
    }
}

pub fn apply(items: &mut Vec<Item>, meta: &mut Meta, u: &Value) {
    let Some(kind) = u.get("sessionUpdate").and_then(Value::as_str) else { return };
    let get = |k: &str| u.get(k).and_then(Value::as_str);
    match kind {
        "user_message_chunk" => {
            // An image from an earlier session is not replayed as pixels, only marked.
            let is_image = u.get("content").and_then(|c| c.get("type")).and_then(Value::as_str) == Some("image");
            if let Some(t) = text_of(u).or(is_image.then_some("[image]")) {
                stream(items, t, |i| if let Item::User(s) = i { Some(s) } else { None }, Item::User);
            }
        }
        "agent_message_chunk" => {
            if let Some(t) = text_of(u) {
                stream(items, t, |i| if let Item::Agent(s) = i { Some(s) } else { None }, Item::Agent);
            }
        }
        "agent_thought_chunk" => {
            if let Some(t) = text_of(u) {
                stream(items, t, |i| if let Item::Thought(s) = i { Some(s) } else { None }, Item::Thought);
            }
        }
        "tool_call" => {
            let tool = Tool {
                id: get("toolCallId").unwrap_or_default().into(),
                title: get("title").unwrap_or("Tool").into(),
                kind: get("kind").unwrap_or("other").into(),
                status: get("status").unwrap_or("pending").into(),
                output: tool_output(u).unwrap_or_default(),
            };
            match items.iter_mut().find_map(|i| if let Item::Tool(t) = i { Some(t).filter(|t| t.id == tool.id) } else { None }) {
                Some(t) => *t = tool,
                None => items.push(Item::Tool(tool)),
            }
        }
        "tool_call_update" => {
            let id = get("toolCallId").unwrap_or_default();
            if let Some(t) = items.iter_mut().rev().find_map(|i| if let Item::Tool(t) = i { Some(t).filter(|t| t.id == id) } else { None }) {
                if let Some(s) = get("title") {
                    t.title = s.into();
                }
                if let Some(s) = get("status") {
                    t.status = s.into();
                }
                if let Some(s) = tool_output(u).filter(|_| get("status").is_some()) {
                    t.output = s;
                }
            }
        }
        "plan" => {
            let entries: Vec<(String, String)> = u
                .get("entries")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|e| Some((e.get("content")?.as_str()?.to_string(), e.get("status")?.as_str()?.to_string())))
                .collect();
            match items.iter_mut().rev().find_map(|i| if let Item::Plan(p) = i { Some(p) } else { None }) {
                Some(p) => *p = entries,
                None => items.push(Item::Plan(entries)),
            }
        }
        "available_commands_update" => {
            meta.commands = u
                .get("availableCommands")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    Some(Command {
                        name: c.get("name")?.as_str()?.to_string(),
                        description: c.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                        hint: c.get("input").and_then(|i| i.get("hint")).and_then(Value::as_str).map(String::from).or_else(|| c.get("input").filter(|i| !i.is_null()).map(|_| String::new())),
                    })
                })
                .collect();
        }
        "config_option_update" => meta.options = parse_options(u.get("configOptions").unwrap_or(&Value::Null)),
        "current_mode_update" => {
            if let (Some(mode), Some(o)) = (get("currentModeId"), meta.options.iter_mut().find(|o| o.id == "mode")) {
                o.current = mode.into();
            }
        }
        "usage_update" => {
            if let (Some(used), Some(size)) = (u.get("used").and_then(Value::as_u64), u.get("size").and_then(Value::as_u64)) {
                meta.context = Some((used, size));
            }
        }
        "session_info_update" => meta.title = get("title").map(String::from).or(meta.title.take()),
        _ => {}
    }
}

/// Commands matching what is typed after the `/`, best first (prefix matches before substring matches).
pub fn matching<'a>(commands: &'a [Command], typed: &str) -> Vec<&'a Command> {
    let q = typed.to_lowercase();
    let mut hits: Vec<(bool, &Command)> = commands
        .iter()
        .filter_map(|c| {
            let n = c.name.to_lowercase();
            // "ponytail:ponytail-help" also answers to "help": the part after the plugin prefix counts as a prefix.
            let short = n.rsplit(':').next().unwrap_or(&n);
            (n.starts_with(&q) || short.starts_with(&q)).then_some((true, c)).or_else(|| n.contains(&q).then_some((false, c)))
        })
        .collect();
    hits.sort_by_key(|(prefix, _)| !prefix);
    hits.into_iter().map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(updates: &[Value]) -> (Vec<Item>, Meta) {
        let (mut items, mut meta) = (vec![], Meta::default());
        for u in updates {
            apply(&mut items, &mut meta, u);
        }
        (items, meta)
    }

    #[test]
    fn streamed_chunks_join_and_a_tool_call_is_updated_in_place() {
        let (items, meta) = run(&[
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hel"}}),
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "lo"}}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Terminal", "kind": "execute", "status": "pending"}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "title": "ls"}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "rawOutput": "a\nb"}),
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Done"}}),
            json!({"sessionUpdate": "usage_update", "used": 28174, "size": 400000}),
            json!({"sessionUpdate": "session_info_update", "title": "Pong"}),
            json!({"sessionUpdate": "something_new"}),
        ]);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Item::Agent("Hello".into()));
        assert_eq!(items[1], Item::Tool(Tool { id: "t1".into(), title: "ls".into(), kind: "execute".into(), status: "completed".into(), output: "a\nb".into() }));
        assert_eq!(items[2], Item::Agent("Done".into()));
        assert_eq!(meta.context, Some((28174, 400000)));
        assert_eq!(meta.title.as_deref(), Some("Pong"));
    }

    #[test]
    fn commands_and_options_come_from_the_agent() {
        let (_, mut meta) = run(&[json!({"sessionUpdate": "available_commands_update", "availableCommands": [
            {"name": "ponytail:ponytail-help", "description": "Quick reference", "input": null},
            {"name": "compact", "description": "Shrink the context", "input": {"hint": "[focus]"}},
            {"name": "review", "description": "Review", "input": null}
        ]})]);
        assert_eq!(meta.commands[1].hint.as_deref(), Some("[focus]"));
        assert_eq!(meta.commands[0].hint, None);
        let names = |q: &str| matching(&meta.commands, q).into_iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("help"), ["ponytail:ponytail-help"]);
        assert_eq!(names("co"), ["compact"]);
        assert_eq!(names("view"), ["review"]);
        assert_eq!(names("").len(), 3);

        meta.options = parse_options(&json!([{"id": "mode", "name": "Mode", "type": "select", "currentValue": "auto", "options": [
            {"value": "default", "name": "Manual"}, {"value": "auto", "name": "Auto"}]}]));
        assert_eq!(meta.options[0].current_label(), "Auto");
        apply(&mut vec![], &mut meta, &json!({"sessionUpdate": "current_mode_update", "currentModeId": "default"}));
        assert_eq!(meta.options[0].current_label(), "Manual");
    }

    #[test]
    fn replayed_history_rebuilds_the_conversation() {
        let (items, _) = run(&[
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "say pong"}}),
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "pong"}}),
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "image", "data": "AAAA", "mimeType": "image/png"}}),
        ]);
        assert_eq!(items, [Item::User("say pong".into()), Item::Agent("pong".into()), Item::User("[image]".into())]);
    }
}
