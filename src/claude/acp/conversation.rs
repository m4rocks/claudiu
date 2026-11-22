use super::*;

/// Sends a task and its live messages in one ACP session, streaming updates as they arrive.
pub(super) fn converse(from_agent: impl BufRead + Send + 'static, to_agent: impl Write, request: &Request, inbox: Option<Inbox>, emit: &mut impl FnMut(Event)) -> Result<String, String> {
    converse_waiting(from_agent, to_agent, request, inbox, emit, BACKGROUND_WAIT)
}

/// `converse` with a choice of how long to wait for a command the agent moved to the background, so a test needn't wait 30 minutes.
pub(super) fn converse_waiting(from_agent: impl BufRead + Send + 'static, mut to_agent: impl Write, request: &Request, mut inbox: Option<Inbox>, emit: &mut impl FnMut(Event), background_wait: Duration) -> Result<String, String> {
    let agent = request.agent;
    let name = agent.name();
    // ACP can go quiet during a long tool call. Reading stdout on its own thread lets the inbox still wake us.
    let (tx, rx) = sync_channel(32);
    std::thread::spawn(move || {
        for line in from_agent.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let send = |to_agent: &mut dyn Write, message: Value| -> Result<(), String> { writeln!(to_agent, "{message}").and_then(|_| to_agent.flush()).map_err(|_| exited(agent)) };
    let mut messages = Messages::default();
    // OpenCode also reports its own subagents' sessions. Their updates aren't this task's report.
    let read = |to_agent: &mut dyn Write, session: Option<&str>, messages: &mut Messages, emit: &mut dyn FnMut(Event), timeout: Option<Duration>| -> Result<Option<Value>, String> {
        let line = match timeout {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => return Err(exited(agent)),
            },
            None => rx.recv().map_err(|_| exited(agent))?,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { return Ok(None) };
        match msg["method"].as_str() {
            Some("session/update") => {
                if session.is_some_and(|s| msg["params"]["sessionId"] == s) {
                    messages.update(agent, &msg["params"]["update"], emit);
                }
            }
            Some("session/request_permission") => {
                let outcome = permission(&msg["params"]["options"], request.edits);
                if !request.edits {
                    messages.refused = Some(msg["params"]["toolCall"]["title"].as_str().unwrap_or("an action").to_string());
                }
                send(to_agent, json!({ "jsonrpc": "2.0", "id": msg["id"], "result": { "outcome": outcome } }))?;
            }
            // A request this client doesn't serve: it offered no file or terminal access.
            Some(_) if msg.get("id").is_some() => {
                send(to_agent, json!({ "jsonrpc": "2.0", "id": msg["id"], "error": { "code": -32601, "message": "method not found" } }))?;
            }
            None if msg["id"].is_i64() => return Ok(Some(msg)),
            _ => {}
        }
        Ok(None)
    };

    let result = |msg: Value| match msg.get("error") {
        Some(error) => Err(error_reason(agent, error)),
        None => Ok(msg["result"].clone()),
    };
    let answer = |to_agent: &mut dyn Write, id: i64, messages: &mut Messages, emit: &mut dyn FnMut(Event)| -> Result<Value, String> {
        loop {
            if let Some(msg) = read(to_agent, None, messages, emit, None)?
                && msg["id"] == id
            {
                return result(msg);
            }
        }
    };
    send(&mut to_agent, initialize())?;
    let initialized = answer(&mut to_agent, 0, &mut messages, emit)?;
    let supports_steering = initialized["_meta"]["steering"]["supported"] == true;
    let mut steer = supports_steering;
    if !agent.provider().inbox_without_steering && !supports_steering {
        inbox = None; // Claude Code keeps its usual queued delivery on older OpenCode versions.
    }

    send(&mut to_agent, new_session(&request.cwd))?;
    let session = answer(&mut to_agent, 1, &mut messages, emit)?;
    let session_id = session["sessionId"].as_str().ok_or(format!("{} opened no session", server(agent)))?.to_string();
    emit(Event::Progress { text: "Session ready. Sending the task.".into() });

    let prompt = |id, texts: Vec<String>| {
        let blocks: Vec<_> = texts.into_iter().map(|text| json!({ "type": "text", "text": text })).collect();
        json!({ "jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": { "sessionId": session_id, "prompt": blocks } })
    };
    let mut next_id = 2;
    // Each answer lists the options as they stand after the change: a new model can offer other thinking levels.
    let mut offered = session["configOptions"].clone();
    for (config, wanted) in (agent.provider().config_options)(request) {
        let Some(value) = offered_value(&offered, config, &wanted) else { continue };
        send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": next_id, "method": "session/set_config_option", "params": { "sessionId": session_id, "configId": config, "value": value } }))?;
        let changed = answer(&mut to_agent, next_id, &mut messages, emit).map_err(|reason| format!("{name} could not set {config}: {reason}"))?;
        if changed["configOptions"].is_array() {
            offered = changed["configOptions"].clone();
        }
        next_id += 1;
    }
    let mut prompt_id = next_id;
    next_id += 1;
    send(&mut to_agent, prompt(prompt_id, vec![request.prompt.clone()]))?;
    if let Some(inbox) = &inbox {
        emit(Event::Inbox { id: inbox.id.to_string(), path: inbox.path.clone() });
    }

    let mut pending = std::collections::VecDeque::new();
    let mut steering = None;
    let mut done = None;
    let mut next_poll = Instant::now();
    let done = loop {
        if let Some(inbox) = &mut inbox
            && (Instant::now() >= next_poll || done.is_some())
        {
            next_poll = Instant::now() + INBOX_POLL;
            if let Some((seq, text)) = inbox.take() {
                pending.push_back((Some(seq), text));
            }
        }

        if steering.is_none() {
            if let Some(reply) = done.take() {
                if pending.is_empty() {
                    break reply;
                }
                // The client owns the next prompt so it can stream the reply and wait for its stop reason.
                prompt_id = next_id;
                next_id += 1;
                let (seqs, texts): (Vec<Option<u64>>, Vec<String>) = pending.drain(..).unzip();
                send(&mut to_agent, prompt(prompt_id, texts))?;
                for seq in seqs.into_iter().flatten() {
                    emit(Event::Accepted { seq });
                }
                messages = Messages::default();
                steer = supports_steering;
                emit(Event::Progress { text: format!("{name} is continuing with the follow-up message.") });
            } else if steer && let Some((seq, text)) = pending.pop_front() {
                let mut params = json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": text }],
                });
                if agent.provider().steering_prompt_required {
                    params["_meta"] = json!({ "steering": { "idleBehavior": "promptRequired" } });
                }
                send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": next_id, "method": "_session/steering", "params": params }))?;
                steering = Some((next_id, seq, text));
                next_id += 1;
            }
        }

        let timeout = inbox.as_ref().map(|_| next_poll.saturating_duration_since(Instant::now()));
        let Some(msg) = read(&mut to_agent, Some(&session_id), &mut messages, emit, timeout)? else { continue };
        if msg["id"] == prompt_id {
            done = Some(result(msg)?);
        } else if steering.as_ref().is_some_and(|(id, _, _)| msg["id"] == *id) {
            let (_, seq, text) = steering.take().unwrap();
            match msg["result"]["outcome"].as_str() {
                Some("injected") => {
                    if let Some(seq) = seq {
                        emit(Event::Accepted { seq });
                    }
                    emit(Event::Progress { text: format!("{name} received the message in its current turn.") });
                }
                // If a different adapter started a turn despite the opt-in, don't send the message twice.
                Some("startedNewTurn") => return Err(format!("{name} started an untracked turn while steering")),
                // The agent answered that its turn is idle, so the follow-up prompt will carry the message; Claude's
                // message can't wait on that for the rest of the turn. After an error it waits for the follow-up.
                _ => {
                    let answered = msg.get("error").is_none();
                    if answered && let Some(seq) = seq {
                        emit(Event::Accepted { seq });
                    }
                    pending.push_front((if answered { None } else { seq }, text));
                    steer = false;
                    emit(Event::Progress { text: format!("{name} will take the message after its current turn.") });
                }
            }
        }
    };
    let stop = done["stopReason"].as_str().unwrap_or("end_turn");
    if stop == "end_turn" && agent.provider().resumes_after_background && messages.running() {
        emit(Event::Progress { text: format!("{name} is waiting for a command running in the background.") });
        // What it wrote while starting the command ("waiting for it to finish") isn't the report.
        messages.close();
        messages.last = None;
        let deadline = Instant::now() + background_wait;
        let mut quiet_since = Instant::now();
        while (messages.running() || !messages.idle || quiet_since.elapsed() < RESUMED_QUIET) && Instant::now() < deadline {
            let updates = messages.updates;
            match read(&mut to_agent, Some(&session_id), &mut messages, emit, Some(INBOX_POLL)) {
                Ok(_) => {}
                // An agent that exits once its commands are done has still finished its turn.
                Err(_) if !messages.running() => break,
                Err(reason) => return Err(reason),
            }
            if messages.updates != updates {
                quiet_since = Instant::now();
            }
        }
        if messages.running() {
            return Err(format!("{name}'s command running in the background didn't finish within the wait"));
        }
    }
    match (stop, messages.last()) {
        ("end_turn", Some(text)) => Ok(text),
        ("end_turn", None) => Err(format!("{name} finished without writing a report")),
        ("cancelled", _) => Err(match messages.refused {
            Some(action) => format!("{name} stopped when read-only mode refused `{action}`. Turn on Allow edits in Settings → Custom subagents to let it."),
            None => format!("{name}'s turn was cancelled"),
        }),
        ("refusal", _) => Err(format!("{name} refused the task")),
        // Out of tokens or turns: what the agent wrote last is still the best report there is.
        (_, Some(text)) => Ok(text),
        (other, None) => Err(format!("{name} stopped ({other}) without writing a report")),
    }
}

