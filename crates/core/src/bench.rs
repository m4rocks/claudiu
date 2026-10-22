//! `bench`: the spike's measurement. Runs coding tasks through the real adapter with and without Claudiu, keeps
//! every number a GO or STOP decision needs, prepares a blind review of the diffs and writes the report.
//!
//! The decision is the human's. The report states the inputs and which bar they clear.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::{Config, Core, Features};
use crate::delegate::Tokens;
use crate::event::kind;
use crate::acp::AdapterCommand;

// ---- tasks, arms and the plan ------------------------------------------------------------------------------------

/// One piece of work given to Claude Code, and how to tell it worked.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// A git repository to clone fresh for every run.
    pub repo: PathBuf,
    pub commit: Option<String>,
    pub prompt: String,
    /// Shell commands run in the clone after the agent is done; all must succeed (build, tests).
    pub checks: Vec<String>,
    /// Part of the Haiku arm.
    #[serde(default)]
    pub haiku: bool,
    pub timeout_secs: u64,
}

/// A configuration under test.
#[derive(Clone, Copy, Debug)]
pub struct Arm {
    pub name: &'static str,
    pub features: Features,
    pub model: Option<&'static str>,
}

pub fn arms() -> Vec<Arm> {
    let full = Features::default();
    vec![
        Arm { name: "stock", features: Features::stock(), model: None },
        Arm { name: "claudiu", features: full, model: None },
        Arm { name: "claudiu-no-firewall", features: Features { firewall: false, ..full }, model: None },
        Arm { name: "claudiu-no-delegation", features: Features { delegation: false, ..full }, model: None },
        Arm { name: "stock-haiku", features: Features::stock(), model: Some("haiku") },
        Arm { name: "claudiu-haiku", features: full, model: Some("haiku") },
    ]
}

pub struct Planned {
    pub task: usize,
    pub arm: Arm,
    pub run: u32,
}

/// Every run of the matrix: each task `repeats` times per arm, the Haiku arms only on the Haiku tasks.
pub fn plan(tasks: &[Task], repeats: u32, only_arm: Option<&str>) -> Vec<Planned> {
    let mut out = Vec::new();
    for (i, task) in tasks.iter().enumerate() {
        for arm in arms().into_iter().filter(|a| only_arm.is_none_or(|n| a.name == n)) {
            if arm.model.is_some() && !task.haiku {
                continue;
            }
            out.extend((0..repeats).map(|run| Planned { task: i, arm, run }));
        }
    }
    out
}

// ---- one run ------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// The agent's turn ended by itself.
    #[default]
    Finished,
    /// Cut off at the task's time limit.
    TimedOut,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Delegation {
    pub profile: String,
    pub cold_start_tokens: u64,
    pub over_budget: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub task: String,
    pub arm: String,
    pub run: u32,
    pub outcome: Outcome,
    pub error: Option<String>,
    /// The task's checks (build, tests) passed. Acceptance also needs a human to accept the diff.
    pub checks_passed: bool,
    pub cost_usd: f64,
    pub tokens: Tokens,
    pub cache_hit_rate: f64,
    pub duration_ms: u64,
    /// How long each hook call took the core, in milliseconds.
    pub hook_ms: Vec<f64>,
    /// Tool outputs the firewall shortened.
    pub compactions: u32,
    /// Writes and commands the guards refused.
    pub refusals: Vec<String>,
    pub guards_off: bool,
    /// When the guards were off, the task was dropped to ask mode.
    pub guards_fallback: bool,
    pub delegations: Vec<Delegation>,
}

impl RunResult {
    pub fn id(&self) -> String {
        format!("{}__{}__{}", self.task, self.arm, self.run)
    }
}

impl Serialize for Tokens {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Tokens {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Tokens, D::Error> {
        let v = Value::deserialize(d)?;
        let get = |k: &str| v[k].as_u64().unwrap_or(0);
        Ok(Tokens { input: get("input"), cache_write: get("cache_write"), cache_read: get("cache_read"), output: get("output") })
    }
}

fn run_shell(dir: &Path, command: &str) -> bool {
    let mut c = if cfg!(windows) { Command::new("cmd") } else { Command::new("sh") };
    c.current_dir(dir).args([if cfg!(windows) { "/C" } else { "-c" }, command]);
    c.output().is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(dir).args(args).output().context("running git")?;
    if !out.status.success() {
        bail!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn p95(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * 0.95).ceil() as usize).clamp(1, v.len()) - 1]
}

/// Runs one task once in one arm, in a fresh clone and a fresh core, and writes `result.json` and `diff.patch`.
pub fn run_one(adapter: &AdapterCommand, out: &Path, task: &Task, arm: &Arm, run: u32, permission_mode: &str) -> Result<RunResult> {
    let mut result = RunResult { task: task.id.clone(), arm: arm.name.into(), run, ..RunResult::default() };
    let dir = out.join("runs").join(result.id());
    let _ = std::fs::remove_dir_all(&dir);
    let work = dir.join("work");
    std::fs::create_dir_all(&dir)?;
    git(&dir, &["clone", "--quiet", &task.repo.to_string_lossy(), "work"])?;
    if let Some(commit) = &task.commit {
        git(&work, &["checkout", "--quiet", commit])?;
    }

    let mut config = Config::new(dir.join("data"), adapter.clone()).with_features(arm.features);
    config.permission_mode = permission_mode.into();
    let core = Core::open(config)?;
    let events = core.subscribe();
    let started = Instant::now();
    let session = core.start_session(&work)?;
    if let Some(model) = arm.model {
        core.set_config(&session, "model", model)?;
    }
    core.send_prompt(&session, &task.prompt)?;
    let deadline = started + Duration::from_secs(task.timeout_secs);
    let mut cancelled_at: Option<Instant> = None;
    loop {
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(e) if e.session == session && matches!(e.kind.as_str(), kind::TURN_ENDED | kind::INTERRUPTED) => {
                result.outcome = if cancelled_at.is_some() { Outcome::TimedOut } else { Outcome::Finished };
                break;
            }
            Ok(e) if e.session == session && matches!(e.kind.as_str(), kind::ERROR | kind::ADAPTER_EXIT) => {
                result.outcome = Outcome::Failed;
                result.error = Some(e.payload.to_string());
                break;
            }
            _ => {}
        }
        if cancelled_at.is_none() && Instant::now() > deadline {
            let _ = core.cancel(&session);
            cancelled_at = Some(Instant::now());
        }
        if cancelled_at.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            result.outcome = Outcome::TimedOut;
            break;
        }
    }
    result.duration_ms = started.elapsed().as_millis() as u64;

    let spend = core.spend(&session);
    (result.cost_usd, result.tokens, result.cache_hit_rate) = (spend.cost_usd, spend.tokens, spend.tokens.cache_hit_rate());
    result.hook_ms = core.hook_latencies_ms();
    for e in core.events(&session)? {
        match e.kind.as_str() {
            kind::FIREWALL => result.compactions += 1,
            kind::GUARD_REFUSED => result.refusals.push(format!("{}: {}", e.payload["tool"].as_str().unwrap_or("?"), e.payload["reason"].as_str().unwrap_or(""))),
            kind::GUARDS_OFF => (result.guards_off, result.guards_fallback) = (true, e.payload["fallback"].as_bool().unwrap_or(false)),
            kind::DELEGATION_ENDED => result.delegations.push(Delegation {
                profile: e.payload["profile"].as_str().unwrap_or("").into(),
                cold_start_tokens: e.payload["cold_start_tokens"].as_u64().unwrap_or(0),
                over_budget: e.payload["over_budget"].as_bool().unwrap_or(false),
            }),
            _ => {}
        }
    }
    core.shutdown();

    // The diff first (checks may build and leave files); new files count.
    let _ = git(&work, &["add", "-A", "-N"]);
    std::fs::write(dir.join("diff.patch"), git(&work, &["diff"]).unwrap_or_default())?;
    result.checks_passed = result.outcome != Outcome::Failed && task.checks.iter().all(|c| run_shell(&work, c));
    std::fs::write(dir.join("result.json"), serde_json::to_string_pretty(&result)?)?;
    Ok(result)
}

/// The whole matrix (or one arm of it). Existing results are kept, so an interrupted bench can be run again.
pub fn run_all(adapter: &AdapterCommand, out: &Path, tasks: &[Task], repeats: u32, only_arm: Option<&str>, permission_mode: &str) -> Result<Vec<RunResult>> {
    let mut results = Vec::new();
    for p in plan(tasks, repeats, only_arm) {
        let task = &tasks[p.task];
        let id = format!("{}__{}__{}", task.id, p.arm.name, p.run);
        let saved = out.join("runs").join(&id).join("result.json");
        if let Some(done) = std::fs::read_to_string(&saved).ok().and_then(|t| serde_json::from_str::<RunResult>(&t).ok()) {
            results.push(done);
            continue;
        }
        println!("run {id}");
        results.push(run_one(adapter, out, task, &p.arm, p.run, permission_mode)?);
    }
    write_modes(out, permission_mode)?;
    Ok(results)
}

/// Every result under `out`, in a stable order.
pub fn load_results(out: &Path) -> Result<Vec<RunResult>> {
    let mut all = Vec::new();
    for e in std::fs::read_dir(out.join("runs"))?.flatten() {
        if let Ok(text) = std::fs::read_to_string(e.path().join("result.json")) {
            all.push(serde_json::from_str::<RunResult>(&text)?);
        }
    }
    all.sort_by_key(RunResult::id);
    Ok(all)
}

fn write_modes(out: &Path, mode: &str) -> Result<()> {
    std::fs::write(out.join("permission_mode.txt"), mode)?;
    Ok(())
}

// ---- blind review -------------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct ReviewEntry {
    id: String,
    accepted: Option<bool>,
    #[serde(default)]
    notes: String,
}

#[derive(Serialize, Deserialize)]
struct RefusalEntry {
    run: String,
    index: usize,
    text: String,
    #[serde(rename = "false")]
    is_false: Option<bool>,
}

/// Writes the diffs of runs that passed their checks under random ids, without saying which arm wrote them.
/// The key that says so is kept apart. An existing `verdicts.json` is never overwritten.
pub fn prepare_review(out: &Path) -> Result<()> {
    let review = out.join("review");
    std::fs::create_dir_all(&review)?;
    let mut runs: Vec<RunResult> = load_results(out)?.into_iter().filter(|r| r.checks_passed).collect();
    // A random order, so the id says nothing either.
    runs.sort_by_key(|_| uuid::Uuid::new_v4());
    let (mut key, mut entries) = (BTreeMap::new(), Vec::new());
    let mut refusals = Vec::new();
    for (n, r) in runs.iter().enumerate() {
        let id = format!("r{:03}", n + 1);
        let diff = std::fs::read_to_string(out.join("runs").join(r.id()).join("diff.patch")).unwrap_or_default();
        std::fs::write(review.join(format!("{}-{id}.diff", r.task)), diff)?;
        key.insert(id.clone(), json!({"task": r.task, "arm": r.arm, "run": r.run, "run_id": r.id()}));
        entries.push(ReviewEntry { id, accepted: None, notes: String::new() });
    }
    for r in load_results(out)? {
        refusals.extend(r.refusals.iter().enumerate().map(|(index, text)| RefusalEntry { run: r.id(), index, text: text.clone(), is_false: None }));
    }
    std::fs::write(review.join("key.json"), serde_json::to_string_pretty(&key)?)?;
    if !review.join("verdicts.json").exists() {
        std::fs::write(review.join("verdicts.json"), serde_json::to_string_pretty(&entries)?)?;
    }
    if !review.join("refusals.json").exists() {
        std::fs::write(review.join("refusals.json"), serde_json::to_string_pretty(&refusals)?)?;
    }
    Ok(())
}

/// The reviewer's verdicts joined with the key: `(run id, accepted)`.
pub fn load_verdicts(out: &Path) -> Result<Vec<(String, bool)>> {
    let review = out.join("review");
    let Ok(text) = std::fs::read_to_string(review.join("verdicts.json")) else { return Ok(Vec::new()) };
    let entries: Vec<ReviewEntry> = serde_json::from_str(&text)?;
    let key: Value = serde_json::from_str(&std::fs::read_to_string(review.join("key.json"))?)?;
    Ok(entries.iter().filter_map(|e| Some((key[&e.id]["run_id"].as_str()?.to_string(), e.accepted?))).collect())
}

/// The reviewer's answers about guard refusals: `(run id, refusal index, was it false)`.
pub fn load_refusal_reviews(out: &Path) -> Vec<(String, usize, bool)> {
    let text = std::fs::read_to_string(out.join("review").join("refusals.json")).unwrap_or_default();
    let entries: Vec<RefusalEntry> = serde_json::from_str(&text).unwrap_or_default();
    entries.into_iter().filter_map(|e| Some((e.run, e.index, e.is_false?))).collect()
}

// ---- the report ---------------------------------------------------------------------------------------------------

/// What the numbers say against the bar. The decision itself is the human's.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Every GO criterion is met.
    GoBarMet,
    /// The saving is 15-30% and nothing else is wrong: the one-time week extension applies.
    Extend,
    /// A STOP condition holds.
    Stop(Vec<String>),
    /// These GO criteria are missed (and no STOP condition holds).
    BarNotMet(Vec<String>),
    /// These need more data or a human answer.
    Incomplete(Vec<String>),
}

pub struct Report {
    pub verdict: Verdict,
    pub text: String,
}

#[derive(Default)]
struct ArmStats {
    runs: usize,
    accepted: usize,
    cost: f64,
    tokens: Tokens,
}

impl ArmStats {
    fn cost_per_accepted(&self) -> Option<f64> {
        (self.accepted > 0).then(|| self.cost / self.accepted as f64)
    }

    fn accepted_rate(&self) -> f64 {
        if self.runs == 0 { 0.0 } else { self.accepted as f64 / self.runs as f64 }
    }
}

const COST_BAR: f64 = 0.70;
const STOP_SAVING: f64 = 0.15;
const EXTEND_SAVING: f64 = 0.30;
const HOOK_P95_MS: f64 = 50.0;
const COLD_START_TOKENS: f64 = 5_000.0;

/// Computes every criterion of the GO and STOP bar from the results and the human's answers.
pub fn evaluate(results: &[RunResult], verdicts: &[(String, bool)], refusal_reviews: &[(String, usize, bool)]) -> Report {
    let accepted = |r: &RunResult| r.checks_passed && verdicts.iter().any(|(id, ok)| *ok && *id == r.id());
    let mut stats: BTreeMap<&str, ArmStats> = BTreeMap::new();
    for r in results {
        let s = stats.entry(r.arm.as_str()).or_default();
        s.runs += 1;
        s.accepted += usize::from(accepted(r));
        s.cost += r.cost_usd;
        s.tokens.add(&r.tokens);
    }
    let family: Vec<&RunResult> = results.iter().filter(|r| r.arm.starts_with("claudiu")).collect();
    let (stock, full) = (stats.get("stock"), stats.get("claudiu"));

    let mut text = String::from("# Spike report\n\n## Arms\n\n| arm | runs | accepted | accepted rate | cost | cost per accepted | cache-hit rate |\n|---|---|---|---|---|---|---|\n");
    for (name, s) in &stats {
        let cpa = s.cost_per_accepted().map_or("no accepted change".to_string(), |c| format!("${c:.3}"));
        text.push_str(&format!("| {name} | {} | {} | {:.0}% | ${:.3} | {cpa} | {:.1}% |\n", s.runs, s.accepted, s.accepted_rate() * 100.0, s.cost, s.tokens.cache_hit_rate() * 100.0));
    }

    let mut stop: Vec<String> = Vec::new();
    // (criterion, what was measured, the bar, met?) - `None` means it cannot be judged yet.
    let mut rows: Vec<(&str, String, &str, Option<bool>)> = Vec::new();
    let mut row = |name: &'static str, value: String, bar: &'static str, state: Option<bool>| rows.push((name, value, bar, state));

    // Cost per accepted change, against stock.
    let ratio = match (full.and_then(ArmStats::cost_per_accepted), stock.and_then(ArmStats::cost_per_accepted)) {
        (Some(f), Some(s)) if s > 0.0 => Some(f / s),
        _ => None,
    };
    row("cost per accepted change", ratio.map_or("no accepted change in one arm".into(), |r| format!("{r:.2} of stock (saving {:.0}%)", (1.0 - r) * 100.0)), "at most 70% of stock", ratio.map(|r| r <= COST_BAR));
    if let Some(r) = ratio.filter(|r| 1.0 - r < STOP_SAVING) {
        stop.push(format!("the saving is {:.0}%, under 15%", (1.0 - r) * 100.0));
    }
    match (full, stock) {
        (Some(f), Some(s)) => {
            row("accepted rate", format!("{:.0}% vs {:.0}%", f.accepted_rate() * 100.0, s.accepted_rate() * 100.0), "not below stock", Some(f.accepted_rate() >= s.accepted_rate()));
            row("cache-hit rate", format!("{:.1}% vs {:.1}%", f.tokens.cache_hit_rate() * 100.0, s.tokens.cache_hit_rate() * 100.0), "not below stock", Some(f.tokens.cache_hit_rate() >= s.tokens.cache_hit_rate()));
        }
        _ => {
            row("accepted rate", "needs stock and claudiu runs".into(), "not below stock", None);
            row("cache-hit rate", "needs stock and claudiu runs".into(), "not below stock", None);
        }
    }

    // Guard refusals need a human to say which were false.
    let total: usize = family.iter().map(|r| r.refusals.len()).sum();
    let mut false_refusals = 0;
    let mut unreviewed = 0;
    for r in &family {
        for i in 0..r.refusals.len() {
            match refusal_reviews.iter().find(|(id, n, _)| *id == r.id() && *n == i) {
                Some((_, _, is_false)) => false_refusals += usize::from(*is_false),
                None => unreviewed += 1,
            }
        }
    }
    let state = if false_refusals > 0 { Some(false) } else if unreviewed > 0 { None } else { Some(true) };
    row("false guard refusals", format!("{false_refusals} of {total} refusals ({unreviewed} not reviewed)"), "zero", state);

    let hooks: Vec<f64> = family.iter().flat_map(|r| r.hook_ms.iter().copied()).collect();
    row("hook p95", format!("{:.1} ms server side, over {} calls", p95(&hooks), hooks.len()), "under 50 ms", (!hooks.is_empty()).then(|| p95(&hooks) < HOOK_P95_MS));

    let colds: Vec<f64> = family.iter().flat_map(|r| r.delegations.iter().map(|d| d.cold_start_tokens as f64)).collect();
    let mean = colds.iter().sum::<f64>() / colds.len().max(1) as f64;
    row("subagent cold start", if colds.is_empty() { "no subagent ran".to_string() } else { format!("mean {mean:.0} tokens over {} subagents", colds.len()) }, "under 5,000 tokens", (!colds.is_empty()).then_some(mean < COLD_START_TOKENS));

    if !family.is_empty() && !results.iter().any(|r| r.arm == "claudiu" && r.compactions > 0) && results.iter().any(|r| r.arm == "claudiu") {
        stop.push("the hooks never compacted anything (a firewall that cannot compact)".into());
    }
    if family.iter().any(|r| r.guards_off && !r.guards_fallback) {
        stop.push("the guards were bypassed with no fallback to ask mode".into());
    }

    text.push_str("\n## GO criteria\n\n| criterion | measured | bar | state |\n|---|---|---|---|\n");
    let (mut failed, mut pending): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for (name, value, bar, state) in &rows {
        let mark = match state {
            Some(true) => "pass",
            Some(false) => "FAIL",
            None => "pending",
        };
        text.push_str(&format!("| {name} | {value} | {bar} | {mark} |\n"));
        match state {
            Some(false) => failed.push(name.to_string()),
            None => pending.push(name.to_string()),
            _ => {}
        }
    }

    let verdict = if !stop.is_empty() {
        Verdict::Stop(stop)
    } else {
        let other: Vec<String> = failed.iter().filter(|n| *n != "cost per accepted change").cloned().collect();
        if !other.is_empty() {
            Verdict::BarNotMet(other)
        } else if failed.iter().any(|n| n == "cost per accepted change") {
            match ratio {
                Some(r) if 1.0 - r < EXTEND_SAVING => Verdict::Extend,
                _ => Verdict::BarNotMet(vec!["cost per accepted change".into()]),
            }
        } else if !pending.is_empty() {
            Verdict::Incomplete(pending)
        } else {
            Verdict::GoBarMet
        }
    };

    text.push_str("\n## Ablations and the Haiku arm\n\n");
    let base = stock.and_then(ArmStats::cost_per_accepted);
    for (name, s) in &stats {
        if *name == "stock" || *name == "claudiu" {
            continue;
        }
        let vs = match (s.cost_per_accepted(), base) {
            (Some(c), Some(b)) if !name.ends_with("haiku") => format!("{:.2} of stock", c / b),
            _ => "see the table".into(),
        };
        text.push_str(&format!("- {name}: {vs}\n"));
    }
    if let (Some(a), Some(b)) = (stats.get("stock-haiku").and_then(ArmStats::cost_per_accepted), stats.get("claudiu-haiku").and_then(ArmStats::cost_per_accepted)) {
        text.push_str(&format!("- claudiu-haiku is {:.2} of stock-haiku per accepted change\n", b / a));
    }

    text.push_str(&format!("\n## Verdict inputs\n\n{}\n", describe(&verdict)));
    Report { verdict, text }
}

fn describe(v: &Verdict) -> String {
    match v {
        Verdict::GoBarMet => "Every GO criterion is met. The decision is yours.".into(),
        Verdict::Extend => "The saving is between 15% and 30% and nothing else is wrong: the one-time week extension applies. The decision is yours.".into(),
        Verdict::Stop(why) => format!("A STOP condition holds: {}. The decision is yours.", why.join("; ")),
        Verdict::BarNotMet(names) => format!("The GO bar is missed on: {}. No STOP condition holds. The decision is yours.", names.join(", ")),
        Verdict::Incomplete(names) => format!("Not enough to judge yet: {}.", names.join(", ")),
    }
}

// ---- versions ----------------------------------------------------------------------------------------------------

/// What the numbers were measured with.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Versions {
    pub adapter: String,
    pub claude: String,
    pub node: String,
    pub core: String,
    pub os: String,
}

fn output_of(program: &str, arg: &str) -> String {
    Command::new(program).arg(arg).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into())
}

impl Versions {
    /// `adapter_package` is the adapter's package.json.
    pub fn detect(adapter_package: &Path, claude: &str) -> Versions {
        let adapter = std::fs::read_to_string(adapter_package).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["version"].as_str().map(String::from)).unwrap_or_else(|| "unknown".into());
        Versions { adapter, claude: output_of(claude, "--version"), node: output_of("node", "--version"), core: env!("CARGO_PKG_VERSION").into(), os: std::env::consts::OS.into() }
    }
}

/// Appends the versions and each arm's configuration to a report.
pub fn with_versions(text: &str, v: &Versions, permission_mode: &str) -> String {
    let mut out = format!("{text}\n## Pinned versions\n\n- adapter: {}\n- claude: {}\n- node: {}\n- claudiu-core: {}\n- os: {}\n\n## Arms\n\n| arm | permission mode | model | hooks | firewall | claudiu server | delegation |\n|---|---|---|---|---|---|---|\n", v.adapter, v.claude, v.node, v.core, v.os);
    for a in arms() {
        let f = a.features;
        out.push_str(&format!("| {} | {permission_mode} | {} | {} | {} | {} | {} |\n", a.name, a.model.unwrap_or("default"), f.hooks, f.firewall, f.mcp, f.delegation));
    }
    out
}

/// The real adapter the app installs, run with the user's own `claude`.
pub fn real_adapter() -> Result<(AdapterCommand, PathBuf, String)> {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support"));
    let package = base.join("Claudiu/data/acp/node_modules/@agentclientprotocol/claude-agent-acp");
    let which = if cfg!(windows) { "where" } else { "which" };
    let found = Command::new(which).arg("claude").output().context("looking for claude")?;
    let claude = String::from_utf8_lossy(&found.stdout).lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).context("claude is not on PATH")?;
    let adapter = AdapterCommand::new("node").arg(package.join("dist/index.js").to_string_lossy()).env("CLAUDE_CODE_EXECUTABLE", &claude);
    Ok((adapter, package.join("package.json"), claude))
}
