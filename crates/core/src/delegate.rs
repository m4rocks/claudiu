//! `delegate`: a task hands a bounded job to a subagent that Claudiu owns.
//!
//! The subagent is an ordinary session started through the adapter, trimmed to one of four fixed profiles, so its
//! cost is measured here (not guessed from a native spawn) and its budget can be enforced. It answers with a
//! receipt: a small JSON block of references the core validates before the parent sees it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::core::{ChildSpec, Engine, Live, SessionInfo};
use crate::event::{SessionId, kind};

/// One of the four fixed roles a subagent can have. They are not settings.
pub struct Profile {
    pub name: &'static str,
    pub model: &'static str,
    pub effort: &'static str,
    /// Claude Code's own tools; the subagent also gets Claudiu's server.
    pub tools: &'static [&'static str],
    /// Uncached input, cache writes and output together; cache reads are free to the budget.
    pub budget: u64,
    /// The most a brief may cost the parent to write, in tokens.
    pub brief_ceiling: usize,
    pub wall: Duration,
    pub max_turns: u32,
    role: &'static str,
}

const READ: &[&str] = &["Read", "Grep", "Glob"];

static PROFILES: [Profile; 4] = [
    Profile {
        name: "scout",
        model: "haiku",
        effort: "low",
        tools: READ,
        budget: 60_000,
        brief_ceiling: 1_500,
        wall: Duration::from_secs(5 * 60),
        max_turns: 20,
        role: "You are a scout. Find facts in the repository and report them; you cannot edit anything. Use repo_map, outline and find_symbol before reading whole files.",
    },
    Profile {
        name: "thinker",
        model: "opus",
        effort: "high",
        tools: READ,
        budget: 80_000,
        brief_ceiling: 3_000,
        wall: Duration::from_secs(10 * 60),
        max_turns: 10,
        role: "You are a thinker. Work out the design question you were given and put a recommendation, with its trade-offs, in the summary. You cannot edit anything.",
    },
    Profile {
        name: "worker",
        model: "sonnet",
        effort: "medium",
        tools: &["Read", "Grep", "Glob", "Bash", "Edit", "Write"],
        budget: 300_000,
        brief_ceiling: 4_000,
        wall: Duration::from_secs(30 * 60),
        max_turns: 60,
        role: "You are a worker. Make the change you were given, run the checks it names, and report what you changed. Do not widen the scope.",
    },
    Profile {
        name: "verifier",
        model: "sonnet",
        effort: "low",
        tools: &["Read", "Grep", "Glob", "Bash"],
        budget: 80_000,
        brief_ceiling: 1_500,
        wall: Duration::from_secs(10 * 60),
        max_turns: 20,
        role: "You are a verifier. Check the claim you were given by running the checks it names. Fix nothing; report what passed and what failed.",
    },
];

const RECEIPT_FORMAT: &str = "When you finish, reply with exactly one fenced block labelled receipt holding JSON: {\"status\":\"done|blocked|failed\",\"summary\":\"under 500 characters\",\"refs\":[\"path:line or handle\"],\"changed\":[\"paths\"]}. Point to files and lines instead of pasting content. Stay inside the scope you were given.";
const RECEIPT_MAX_TOKENS: usize = 1_500;
const GRACE_TIME: Duration = Duration::from_secs(90);

impl Profile {
    pub fn named(name: &str) -> Option<&'static Profile> {
        PROFILES.iter().find(|p| p.name == name)
    }

    pub fn names() -> String {
        PROFILES.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
    }

    /// The trim recipe: no user settings, only our server, a short system prompt, the profile's tools, a title.
    pub(crate) fn trim(&self, options: &mut Value, new: &mut Value, title: &str) {
        options["tools"] = json!(self.tools);
        options["settingSources"] = json!([]);
        options["strictMcpConfig"] = json!(true);
        options["promptSuggestions"] = json!(false);
        options["title"] = json!(title);
        options["maxTurns"] = json!(self.max_turns);
        new["systemPrompt"] = json!({"mode": "append", "content": [{"type": "text", "text": format!("{} {RECEIPT_FORMAT}", self.role)}]});
    }
}

/// What the parent writes down for a subagent. Long content goes in files and comes as pointers.
struct Brief {
    goal: String,
    done: String,
    scope: Option<String>,
    constraints: Option<String>,
    decided: Option<String>,
    pointers: Vec<String>,
}

impl Brief {
    fn from(args: &Value) -> Result<Brief, String> {
        let text = |key: &str| match &args[key] {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Array(items) => {
                let joined = items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
                (!joined.is_empty()).then_some(joined)
            }
            _ => None,
        };
        let (Some(goal), Some(done)) = (text("goal"), text("done")) else {
            return Err("delegate needs a goal and a done condition.".into());
        };
        let pointers = args["pointers"].as_array().map(|p| p.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        Ok(Brief { goal, done, scope: text("scope"), constraints: text("constraints"), decided: text("decided"), pointers })
    }

    fn render(&self) -> String {
        let mut out = format!("Goal: {}\nDone when: {}\n", self.goal, self.done);
        for (label, value) in [("Scope", &self.scope), ("Constraints", &self.constraints), ("Already decided", &self.decided)] {
            if let Some(v) = value {
                out.push_str(&format!("{label}: {v}\n"));
            }
        }
        if !self.pointers.is_empty() {
            out.push_str(&format!("Start from: {}\n", self.pointers.join(", ")));
        }
        out
    }
}

/// What a subagent hands back.
struct Receipt {
    status: String,
    summary: String,
    refs: Vec<String>,
    changed: Vec<String>,
    tokens: usize,
}

impl Receipt {
    /// The last receipt block in `text`, checked against the schema.
    fn parse(text: &str) -> Result<Receipt, String> {
        let start = text.rfind("```receipt").ok_or("there is no ```receipt block in your reply")?;
        let body = &text[start + "```receipt".len()..];
        let body = body.split("```").next().unwrap_or(body).trim();
        let tokens = body.len() / 4;
        if tokens > RECEIPT_MAX_TOKENS {
            return Err(format!("the receipt is about {tokens} tokens; the limit is {RECEIPT_MAX_TOKENS}. Use references"));
        }
        let v: Value = serde_json::from_str(body).map_err(|e| format!("the receipt is not valid JSON ({e})"))?;
        let status = match v["status"].as_str() {
            Some(s @ ("done" | "blocked" | "failed")) => s.to_string(),
            _ => return Err("status must be done, blocked or failed".into()),
        };
        let summary = v["summary"].as_str().filter(|s| !s.trim().is_empty()).ok_or("summary is missing")?.to_string();
        if summary.len() > 800 {
            return Err("summary is over 800 characters; shorten it and use refs".into());
        }
        let list = |key: &str, required: bool| -> Result<Vec<String>, String> {
            match v.get(key) {
                Some(Value::Array(items)) if items.iter().all(Value::is_string) => Ok(items.iter().filter_map(Value::as_str).map(String::from).collect()),
                None if !required => Ok(Vec::new()),
                _ => Err(format!("{key} must be a list of strings")),
            }
        };
        Ok(Receipt { status, summary, refs: list("refs", true)?, changed: list("changed", false)?, tokens })
    }
}

/// A flag that waits: set once, waited on by siblings.
#[derive(Default)]
pub struct Signal {
    set: Mutex<bool>,
    cv: Condvar,
}

impl Signal {
    pub fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.cv.notify_all();
    }

    /// Waits until set or `timeout` passes.
    fn wait(&self, timeout: Duration) {
        let guard = self.set.lock().unwrap();
        let _ = self.cv.wait_timeout_while(guard, timeout, |set| !*set);
    }
}

type Gate = Arc<Mutex<Option<(Arc<Signal>, Instant)>>>;

/// Same-profile siblings of one task start one at a time, each after the previous one's first streamed chunk
/// (its prefix is then cached) or after the fallback, so parallel children do not all pay a cache write.
#[derive(Default)]
pub struct Stagger(Mutex<HashMap<(SessionId, &'static str), Gate>>);

impl Stagger {
    fn gate(&self, parent: &SessionId, profile: &'static str) -> Gate {
        self.0.lock().unwrap().entry((parent.clone(), profile)).or_default().clone()
    }
}

#[derive(Clone, Copy, Default)]
pub struct Tokens {
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
}

impl Tokens {
    fn billed(&self) -> u64 {
        self.input + self.cache_write + self.output
    }

    fn json(&self) -> Value {
        json!({"input": self.input, "cache_write": self.cache_write, "cache_read": self.cache_read, "output": self.output})
    }
}

/// What the adapter says a session has used. A message streams in several updates that each repeat its usage,
/// so each message counts once, at its largest.
#[derive(Default)]
pub struct Meter(Mutex<(Vec<String>, HashMap<String, Tokens>)>);

impl Meter {
    pub fn add(&self, message: &Value) {
        if message["type"] != "assistant" {
            return;
        }
        let (Some(id), usage) = (message["message"]["id"].as_str(), &message["message"]["usage"]) else { return };
        let get = |key: &str| usage[key].as_u64().unwrap_or(0);
        let (order, by_id) = &mut *self.0.lock().unwrap();
        if !by_id.contains_key(id) {
            order.push(id.to_string());
        }
        let seen = by_id.entry(id.to_string()).or_default();
        seen.input = seen.input.max(get("input_tokens"));
        seen.cache_write = seen.cache_write.max(get("cache_creation_input_tokens"));
        seen.cache_read = seen.cache_read.max(get("cache_read_input_tokens"));
        seen.output = seen.output.max(get("output_tokens"));
    }

    fn totals(&self) -> Tokens {
        let guard = self.0.lock().unwrap();
        guard.1.values().fold(Tokens::default(), |a, t| Tokens {
            input: a.input + t.input,
            cache_write: a.cache_write + t.cache_write,
            cache_read: a.cache_read + t.cache_read,
            output: a.output + t.output,
        })
    }

    pub fn billed(&self) -> u64 {
        self.totals().billed()
    }

    /// What the first request wrote to the cache: the price of starting cold.
    fn cold_start_write(&self) -> u64 {
        let guard = self.0.lock().unwrap();
        guard.0.first().and_then(|id| guard.1.get(id)).map_or(0, |t| t.cache_write)
    }
}

/// One delegation in flight.
pub struct Run {
    pub id: String,
    child: OnceLock<SessionId>,
    cancelled: AtomicBool,
}

impl Run {
    fn new(id: String) -> Run {
        Run { id, child: OnceLock::new(), cancelled: AtomicBool::new(false) }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn child(&self) -> Option<&SessionId> {
        self.child.get()
    }
}

#[derive(PartialEq)]
enum Cut {
    Cancelled,
    Budget,
    Wall,
}

struct TurnEnd {
    /// Why Claudiu stopped the turn, if it did.
    cut: Option<Cut>,
    stop: Option<String>,
    error: Option<String>,
}

fn kilo(n: u64) -> String {
    if n >= 1_000 { format!("{:.1}k", n as f64 / 1_000.0) } else { n.to_string() }
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

impl Engine {
    /// The `delegate` tool. Waits for the receipt, or with `background` returns at once and wakes the parent later.
    pub(crate) fn delegate(self: &Arc<Engine>, parent: &SessionId, info: &Arc<SessionInfo>, args: &Value) -> String {
        if info.parent.is_some() {
            return "Subagents cannot delegate.".into();
        }
        let Some(profile) = Profile::named(args["profile"].as_str().unwrap_or("")) else {
            return format!("Unknown profile. Use {}.", Profile::names());
        };
        let brief = match Brief::from(args) {
            Ok(b) => b,
            Err(refusal) => return refusal,
        };
        let tokens = brief.render().len() / 4;
        if tokens > profile.brief_ceiling {
            return format!("The brief is about {tokens} tokens; the {} ceiling is {}. Put the content in files and pass pointers.", profile.name, profile.brief_ceiling);
        }
        if info.guards_off() {
            return "Delegation is off for this task because its guards are not running.".into();
        }
        let run = Arc::new(Run::new(format!("d{}", self.next_delegation.fetch_add(1, Ordering::SeqCst))));
        info.runs.lock().unwrap().push(run.clone());
        if args["background"] == true {
            let (engine, parent, info, id) = (self.clone(), parent.clone(), info.clone(), run.id.clone());
            std::thread::spawn(move || {
                let result = engine.drive(&parent, &info, profile, &brief, &run);
                engine.wake(&parent, &format!("Delegation {} finished. {result}", run.id));
            });
            return format!("Started {} delegation {id}. You will be woken with its receipt when it finishes.", profile.name);
        }
        self.drive(parent, info, profile, &brief, &run)
    }

    /// Starts the subagent, runs it to a receipt (or a cut), records the delegation and returns what the parent reads.
    fn drive(&self, parent: &SessionId, info: &Arc<SessionInfo>, profile: &'static Profile, brief: &Brief, run: &Arc<Run>) -> String {
        let prompt = brief.render();
        let title = format!("{}: {}", profile.name, clip(&brief.goal, 60));
        let finish = |text: String| {
            info.runs.lock().unwrap().retain(|r| !Arc::ptr_eq(r, run));
            text
        };

        let child = match self.start_staggered(parent, info, profile, &title) {
            Ok(child) => child,
            Err(e) => return finish(format!("Could not start the {} subagent: {e}", profile.name)),
        };
        let _ = run.child.set(child.clone());
        let _ = self.rec.record(
            parent,
            kind::DELEGATION_STARTED,
            json!({"id": run.id, "profile": profile.name, "child": child.to_string(), "brief_tokens": prompt.len() / 4, "budget": profile.budget}),
        );

        let result = self.run_to_receipt(run, &child, profile, prompt);

        let (Some(child_info), tokens) = (self.shared.session(&child), result.tokens) else { return finish("The subagent vanished.".into()) };
        let _ = self.rec.record(
            parent,
            kind::DELEGATION_ENDED,
            json!({
                "id": run.id, "profile": profile.name, "child": child.to_string(), "status": result.status, "turns": result.turns,
                "tool_calls": child_info.tool_calls.load(Ordering::SeqCst), "over_budget": tokens.billed() > profile.budget,
                "cold_start_cache_write": child_info.meter.cold_start_write(), "tokens": tokens.json(),
                "receipt_tokens": result.receipt.as_ref().map_or(0, |r| r.tokens),
            }),
        );
        self.end_session(&child);
        finish(result.text(profile))
    }

    fn start_staggered(&self, parent: &SessionId, info: &Arc<SessionInfo>, profile: &'static Profile, title: &str) -> anyhow::Result<SessionId> {
        let gate = self.stagger.gate(parent, profile.name);
        let mut last = gate.lock().unwrap();
        if let Some((streamed, started)) = last.as_ref() {
            streamed.wait(self.stagger_fallback.saturating_sub(started.elapsed()));
        }
        let child = self.start(&info.cwd, Some(ChildSpec { parent: parent.clone(), profile, title: title.to_string() }))?;
        let streamed = self.shared.session(&child).map(|i| i.streamed.clone()).unwrap_or_default();
        *last = Some((streamed, Instant::now()));
        Ok(child)
    }

    /// The turns of one subagent: the brief, a grace prompt if it is cut, one retry for a bad receipt.
    fn run_to_receipt(&self, run: &Run, child: &SessionId, profile: &'static Profile, brief: String) -> Outcome {
        let (Some(info), Ok(live)) = (self.shared.session(child), self.live(child)) else {
            return Outcome::failed(0, Tokens::default(), "the subagent session is gone".into());
        };
        let mut prompt = brief;
        let (mut limit, mut deadline) = (profile.budget, Instant::now() + profile.wall);
        let (mut turns, mut retried) = (0, false);
        let mut cut_status: Option<&'static str> = None;
        loop {
            if run.cancelled() {
                return Outcome { status: "cancelled".into(), turns, tokens: info.meter.totals(), receipt: None, why: None };
            }
            turns += 1;
            let end = self.run_turn(run, child, &info, &live, &prompt, limit, deadline);
            let tokens = info.meter.totals();
            if run.cancelled() {
                return Outcome { status: "cancelled".into(), turns, tokens, receipt: None, why: None };
            }
            if let Some(why) = end.error {
                return Outcome::failed(turns, tokens, why);
            }
            let we_stopped_it = end.stop.as_deref() == Some("cancelled") && matches!(end.cut, Some(Cut::Budget | Cut::Wall));
            if we_stopped_it && cut_status.is_none() {
                let (status, said) = if end.cut == Some(Cut::Budget) {
                    ("over_budget", "You are over your token budget. Stop now and reply with only your receipt block.")
                } else {
                    ("timed_out", "You are out of time. Stop now and reply with only your receipt block.")
                };
                cut_status = Some(status);
                // The grace turn gets a little room to write the receipt, and a short clock.
                limit = tokens.billed() + (profile.budget / 10).max(3_000);
                deadline = Instant::now() + GRACE_TIME;
                prompt = said.into();
                continue;
            }
            if we_stopped_it {
                return Outcome { status: cut_status.unwrap_or("failed").into(), turns, tokens, receipt: None, why: Some("no receipt within the grace turn".into()) };
            }
            let text = info.text.lock().unwrap().clone();
            match Receipt::parse(&text) {
                Ok(r) => return Outcome { status: cut_status.map_or_else(|| r.status.clone(), String::from), turns, tokens, receipt: Some(r), why: None },
                Err(why) if cut_status.is_none() && !retried => {
                    retried = true;
                    prompt = format!("Your receipt was not accepted: {why}. Reply with only a corrected ```receipt block.");
                }
                Err(why) => return Outcome { status: cut_status.unwrap_or("failed").into(), turns, tokens, receipt: None, why: Some(why) },
            }
        }
    }

    /// One prompt, watched: stopped when the task is interrupted, the budget is spent or the clock runs out.
    #[allow(clippy::too_many_arguments)]
    fn run_turn(&self, run: &Run, child: &SessionId, info: &SessionInfo, live: &Arc<Live>, prompt: &str, limit: u64, deadline: Instant) -> TurnEnd {
        let (tx, rx) = channel();
        if let Err(e) = self.start_turn(child, live, prompt, move |result| {
            let _ = tx.send(result);
        }) {
            return TurnEnd { cut: None, stop: None, error: Some(e.to_string()) };
        }
        let mut cut = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Ok(answer)) => return TurnEnd { cut, stop: answer["stopReason"].as_str().map(String::from), error: None },
                Ok(Err(e)) => return TurnEnd { cut, stop: None, error: Some(e) },
                Err(RecvTimeoutError::Timeout) if cut.is_none() => {
                    cut = if run.cancelled() {
                        Some(Cut::Cancelled)
                    } else if info.meter.billed() > limit {
                        Some(Cut::Budget)
                    } else if Instant::now() > deadline {
                        Some(Cut::Wall)
                    } else {
                        None
                    };
                    if cut.is_some() {
                        let _ = self.cancel(child);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return TurnEnd { cut, stop: None, error: Some("the adapter went away".into()) },
            }
        }
    }

    /// Tells the parent a background delegation finished, as soon as it is not in the middle of a turn.
    fn wake(&self, parent: &SessionId, text: &str) {
        for _ in 0..3_000 {
            let Ok(live) = self.live(parent) else { return };
            if self.start_turn(parent, &live, text, |_| {}).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

struct Outcome {
    status: String,
    turns: u32,
    tokens: Tokens,
    receipt: Option<Receipt>,
    why: Option<String>,
}

impl Outcome {
    fn failed(turns: u32, tokens: Tokens, why: String) -> Outcome {
        Outcome { status: "failed".into(), turns, tokens, receipt: None, why: Some(why) }
    }

    /// What the parent reads: short, and mostly references.
    fn text(&self, profile: &Profile) -> String {
        let name = profile.name;
        let spent = format!("{} of {} tokens, {} turn{}", kilo(self.tokens.billed()), kilo(profile.budget), self.turns, if self.turns == 1 { "" } else { "s" });
        let lead = match self.status.as_str() {
            "cancelled" => return format!("{name} cancelled."),
            "over_budget" => format!("{name} went over budget ({spent})"),
            "timed_out" => format!("{name} ran out of time ({spent})"),
            "failed" if self.receipt.is_none() => format!("{name} failed"),
            other => format!("{name} {other} ({spent})"),
        };
        let Some(r) = &self.receipt else {
            return format!("{lead}: {}. No valid receipt after one retry; its transcript is in Claudiu.", self.why.as_deref().unwrap_or("no reason given"));
        };
        let mut out = format!("{lead}: {}", r.summary);
        if !r.refs.is_empty() {
            out.push_str(&format!("\nrefs: {}", r.refs.join(", ")));
        }
        if !r.changed.is_empty() {
            out.push_str(&format!("\nchanged: {}", r.changed.join(", ")));
        }
        out
    }
}
