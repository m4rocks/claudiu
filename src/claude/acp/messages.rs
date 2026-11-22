use super::*;

/// The messages the agent wrote in this run, as their pieces arrive. A message ends where a tool call starts or
/// another message begins.
#[derive(Default)]
pub(super) struct Messages {
    current: String,
    current_id: Option<String>,
    pub(super) last: Option<String>,
    tools: std::collections::HashMap<String, ToolProgress>,
    /// How many updates arrived, so a wait can tell when the agent went quiet.
    pub(super) updates: u64,
    /// Whether the last update was a usage report with no tool running, which agy sends as a step's last update.
    pub(super) idle: bool,
    // Codex sometimes offers only "cancel" for a refusal, which stops the turn.
    pub(super) refused: Option<String>,
}

#[derive(Default)]
struct ToolProgress {
    title: String,
    waiting_for_target: bool,
    status: String,
    output: String,
}

/// What a tool call acts on, from its input: the command, the pattern searched for, the address, or else the file.
pub(super) fn tool_target(input: &Value) -> Option<&str> {
    ["command", "pattern", "query", "url", "path", "Path", "filePath"].iter().find_map(|key| input[key].as_str()).filter(|t| !t.is_empty())
}

impl Messages {
    pub(super) fn update(&mut self, agent: Agent, update: &Value, emit: &mut dyn FnMut(Event)) {
        self.updates += 1;
        self.idle = update["sessionUpdate"] == "usage_update" && !self.running();
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let Some(text) = update["content"]["text"].as_str() else { return };
                let id = update["messageId"].as_str().map(String::from);
                if id.is_some() && id != self.current_id {
                    self.close();
                    self.current_id = id;
                }
                self.current.push_str(text);
                emit(Event::Text { text: text.to_string() });
            }
            Some("agent_thought_chunk") => {
                if let Some(text) = update["content"]["text"].as_str() {
                    emit(Event::Thought { text: text.into() });
                }
            }
            Some("plan") => {
                let entries: Vec<_> = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| Some(format!("- {} ({})", entry["content"].as_str()?, entry["status"].as_str().unwrap_or("pending").replace('_', " "))))
                    .collect();
                if !entries.is_empty() {
                    emit(Event::Progress { text: format!("Plan\n{}", entries.join("\n")) });
                }
            }
            Some("tool_call") => {
                self.close();
                self.tool_progress(agent, update, true, emit);
            }
            Some("tool_call_update") => self.tool_progress(agent, update, false, emit),
            _ => {}
        }
    }

    fn tool_progress(&mut self, agent: Agent, update: &Value, started: bool, emit: &mut dyn FnMut(Event)) {
        let id = update["toolCallId"].as_str();
        let mut untracked = ToolProgress::default();
        let tool = match id {
            Some(id) => self.tools.entry(id.into()).or_default(),
            None => &mut untracked,
        };
        let target = tool_target(&update["rawInput"]);
        if started {
            let title = update["title"].as_str().filter(|title| !title.is_empty()).unwrap_or("a tool");
            tool.title = title.into();
            // OpenCode may start with only the tool name and supply its target in the first update.
            tool.waiting_for_target = agent.provider().deferred_tool_target && !title.contains(' ') && target.is_none() && id.is_some();
            if !tool.waiting_for_target {
                if let Some(target) = target.filter(|_| !title.contains(' ')) {
                    tool.title = format!("{title} {target}");
                }
                emit(Event::Step { text: tool.title.clone() });
            }
        } else if tool.waiting_for_target {
            tool.waiting_for_target = false;
            if let Some(target) = target {
                tool.title = format!("{} {target}", tool.title);
            }
            emit(Event::Step { text: tool.title.clone() });
        } else if let Some(title) = update["title"].as_str().filter(|title| !title.is_empty()) {
            tool.title = title.into();
        }
        if let Some(status) = update["status"].as_str().filter(|status| *status != tool.status) {
            tool.status = status.into();
            if !started || status == "failed" {
                let title = if tool.title.is_empty() { id.unwrap_or("Tool") } else { &tool.title };
                emit(Event::Progress { text: format!("{title} · {}", status.replace('_', " ")) });
            }
        }
        let metadata = &update["_meta"];
        for key in ["terminal_output_delta", "mcp_output_delta"] {
            if let Some(text) = metadata[key]["data"].as_str().filter(|text| !text.is_empty()) {
                emit(Event::Output { text: text.into() });
            }
        }
        let content: Vec<_> = update["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == "content" && item["content"]["type"] == "text")
            .filter_map(|item| item["content"]["text"].as_str())
            .collect();
        // On Codex starts, ACP content may describe the input. Result snapshots arrive in updates.
        let output = if started || metadata["terminal_output_delta"].is_object() || metadata["mcp_output_delta"].is_object() {
            None
        } else if content.is_empty() {
            let raw = &update["rawOutput"];
            raw.as_str().or_else(|| raw["formatted_output"].as_str()).or_else(|| raw["output"].as_str()).map(str::to_owned)
        } else {
            Some(content.join("\n"))
        };
        if let Some(output) = output {
            let added = output.strip_prefix(&tool.output).unwrap_or(&output);
            if !added.is_empty() {
                emit(Event::Output { text: added.into() });
            }
            tool.output = output;
        }
    }

    /// Ends the message being written; a blank one doesn't replace the last.
    pub(super) fn close(&mut self) {
        let text = std::mem::take(&mut self.current);
        if !text.trim().is_empty() {
            self.last = Some(text);
        }
        self.current_id = None;
    }

    /// Whether a tool call has started and not yet finished.
    pub(super) fn running(&self) -> bool {
        self.tools.values().any(|tool| matches!(tool.status.as_str(), "pending" | "in_progress"))
    }

    pub(super) fn last(&mut self) -> Option<String> {
        self.close();
        self.last.clone()
    }
}

