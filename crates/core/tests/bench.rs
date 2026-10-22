use std::path::Path;
use std::process::Command;

use claudiu_core::AdapterCommand;
use claudiu_core::bench::{self, Outcome, RunResult, Task, Verdict};
use serde_json::{Value, json};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").current_dir(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

fn repo(dir: &Path) -> std::path::PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

fn task(id: &str, repo: &Path, haiku: bool) -> Task {
    Task { id: id.into(), repo: repo.to_path_buf(), commit: None, prompt: "Add a fix.".into(), checks: vec!["git rev-parse --verify HEAD".into()], haiku, timeout_secs: 60 }
}

fn fake(dir: &Path, script: &Value) -> AdapterCommand {
    let path = dir.join("script.json");
    std::fs::write(&path, script.to_string()).unwrap();
    AdapterCommand::new(env!("CARGO_BIN_EXE_fake-acp")).env("FAKE_ACP_SCRIPT", path).env("FAKE_ACP_LOG", dir.join("fake.log"))
}

fn agent_turn() -> Value {
    json!({"turns": [{"steps": [
        {"write": {"path": "fix.txt", "text": "the fix\n"}},
        {"notify": {"method": "_claude/sdkMessage", "params": {"message": {"type": "assistant", "message": {"id": "m1", "usage": {
            "input_tokens": 10, "cache_creation_input_tokens": 1000, "cache_read_input_tokens": 9000, "output_tokens": 100}}}}}},
        {"update": {"sessionUpdate": "usage_update", "used": 10000, "size": 1000000, "cost": {"amount": 0.5, "currency": "USD"}}},
        {"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "done"}}},
    ]}]})
}

#[test]
fn the_matrix_is_nine_tasks_three_runs_four_arms_plus_a_haiku_arm_on_four_tasks() {
    let dir = repo_dir();
    let tasks: Vec<Task> = (0..9).map(|i| task(&format!("t{i}"), dir.path(), i < 4)).collect();
    let plan = bench::plan(&tasks, 3, None);
    assert_eq!(plan.len(), 9 * 3 * 4 + 4 * 3 * 2);
    assert!(plan.iter().filter(|p| p.arm.name.ends_with("haiku")).all(|p| tasks[p.task].haiku));
    let names: std::collections::BTreeSet<_> = plan.iter().map(|p| p.arm.name).collect();
    assert_eq!(names.into_iter().collect::<Vec<_>>(), ["claudiu", "claudiu-haiku", "claudiu-no-delegation", "claudiu-no-firewall", "stock", "stock-haiku"]);
    assert_eq!(bench::plan(&tasks, 3, Some("stock")).len(), 27);
}

fn repo_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn every_arm_runs_a_task_end_to_end_and_leaves_machine_readable_results() {
    let dir = repo_dir();
    let r = repo(dir.path());
    let out = dir.path().join("out");
    let adapter = fake(dir.path(), &agent_turn());

    let results = bench::run_all(&adapter, &out, &[task("fix-it", &r, true)], 1, None, "auto").unwrap();

    assert_eq!(results.len(), 6, "four arms and two haiku arms");
    for res in &results {
        assert_eq!(res.outcome, Outcome::Finished, "{}", res.arm);
        assert!(res.checks_passed);
        assert_eq!(res.cost_usd, 0.5);
        assert_eq!(res.tokens.cache_read, 9000);
        assert!((res.cache_hit_rate - 0.9).abs() < 0.01);
        let on_disk: RunResult = serde_json::from_str(&std::fs::read_to_string(out.join("runs").join(res.id()).join("result.json")).unwrap()).unwrap();
        assert_eq!(&on_disk, res);
        assert!(std::fs::read_to_string(out.join("runs").join(res.id()).join("diff.patch")).unwrap().contains("fix.txt"), "the agent's change is the diff");
    }

    // The baseline is Claude Code alone; the full arm has the hooks and the server.
    let news: Vec<Value> = std::fs::read_to_string(dir.path().join("fake.log")).unwrap().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|m| m["method"] == "session/new").map(|m| m["params"].clone()).collect();
    let bare = news.iter().filter(|n| n["mcpServers"].as_array().unwrap().is_empty()).count();
    assert_eq!(bare, 2, "stock and stock-haiku start with no server and no hooks");
    assert!(news.iter().filter(|n| n["_meta"]["claudeCode"]["options"].get("settings").is_none()).count() == 2);
    let firewall_off = news.iter().filter(|n| n["_meta"]["claudeCode"]["options"]["settings"]["hooks"].get("PostToolUse").is_none() && n["_meta"]["claudeCode"]["options"].get("settings").is_some()).count();
    assert_eq!(firewall_off, 1, "only the firewall ablation runs hooks without the firewall");
    let haiku = std::fs::read_to_string(dir.path().join("fake.log")).unwrap().matches("\"value\":\"haiku\"").count();
    assert_eq!(haiku, 2, "the haiku arms set the model");
}

#[test]
fn blind_review_shows_diffs_without_saying_which_arm_wrote_them() {
    let dir = repo_dir();
    let r = repo(dir.path());
    let out = dir.path().join("out");
    bench::run_all(&fake(dir.path(), &agent_turn()), &out, &[task("fix-it", &r, false)], 1, None, "auto").unwrap();

    bench::prepare_review(&out).unwrap();

    let review = out.join("review");
    let mut shown = Vec::new();
    for e in std::fs::read_dir(&review).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".diff") {
            let text = std::fs::read_to_string(e.path()).unwrap();
            assert!(!name.contains("stock") && !name.contains("claudiu"), "{name}");
            assert!(!text.contains("stock") && !text.contains("claudiu"), "{name}");
            shown.push(name);
        }
    }
    assert_eq!(shown.len(), 4, "one diff per run that passed its checks");
    let key: Value = serde_json::from_str(&std::fs::read_to_string(review.join("key.json")).unwrap()).unwrap();
    assert_eq!(key.as_object().unwrap().len(), 4);
    assert!(key.to_string().contains("claudiu-no-firewall"), "the key, kept apart, says who wrote what");
    let verdicts: Value = serde_json::from_str(&std::fs::read_to_string(review.join("verdicts.json")).unwrap()).unwrap();
    assert!(verdicts.as_array().unwrap().iter().all(|v| v["accepted"].is_null()));
}

// ---- the report ------------------------------------------------------------------------------------------------

fn res(arm: &str, run: u32, cost: f64, read: u64, write: u64) -> RunResult {
    RunResult {
        task: "t".into(),
        arm: arm.into(),
        run,
        outcome: Outcome::Finished,
        checks_passed: true,
        cost_usd: cost,
        tokens: claudiu_core::Tokens { input: 10, cache_write: write, cache_read: read, output: 100 },
        cache_hit_rate: 0.0,
        hook_ms: if arm == "stock" { vec![] } else { vec![1.0, 2.0, 3.0] },
        compactions: if arm == "stock" { 0 } else { 4 },
        ..RunResult::default()
    }
}

fn accept_all(results: &[RunResult]) -> Vec<(String, bool)> {
    results.iter().map(|r| (r.id(), true)).collect()
}

fn matrix(claudiu_cost: f64) -> Vec<RunResult> {
    let mut all = Vec::new();
    for run in 0..3 {
        all.push(res("stock", run, 1.0, 5000, 1000));
        all.push(res("claudiu", run, claudiu_cost, 9000, 500));
    }
    all
}

#[test]
fn the_report_checks_every_go_criterion_against_the_bar() {
    let mut all = matrix(0.6);
    all[1].delegations.push(bench::Delegation { profile: "scout".into(), cold_start_tokens: 3000, over_budget: false });
    let report = bench::evaluate(&all, &accept_all(&all), &[]);

    assert_eq!(report.verdict, Verdict::GoBarMet, "{}", report.text);
    for needle in ["cost per accepted change", "accepted rate", "cache-hit rate", "false guard refusals", "hook p95", "subagent cold start", "0.60", "70%"] {
        assert!(report.text.contains(needle), "{needle} missing:\n{}", report.text);
    }
}

#[test]
fn each_missed_criterion_is_named() {
    let all = matrix(0.8);
    let report = bench::evaluate(&all, &accept_all(&all), &[]);
    assert!(matches!(&report.verdict, Verdict::Extend), "20% saving is in the extension range: {:?}", report.verdict);

    let mut slow = matrix(0.5);
    slow[1].hook_ms = vec![80.0; 20];
    let report = bench::evaluate(&slow, &accept_all(&slow), &[]);
    assert_eq!(report.verdict, Verdict::BarNotMet(vec!["hook p95".into()]), "{}", report.text);

    let mut worse = matrix(0.5);
    for r in worse.iter_mut().filter(|r| r.arm == "claudiu") {
        r.tokens.cache_read = 100;
    }
    let report = bench::evaluate(&worse, &accept_all(&worse), &[]);
    assert_eq!(report.verdict, Verdict::BarNotMet(vec!["cache-hit rate".into()]));
}

#[test]
fn a_small_saving_or_a_dead_firewall_or_unguarded_writes_mean_stop() {
    let all = matrix(0.9);
    assert!(matches!(bench::evaluate(&all, &accept_all(&all), &[]).verdict, Verdict::Stop(_)), "10% saving");

    let mut idle = matrix(0.5);
    idle.iter_mut().for_each(|r| r.compactions = 0);
    let Verdict::Stop(why) = bench::evaluate(&idle, &accept_all(&idle), &[]).verdict else { panic!("a firewall that never compacts") };
    assert!(why.iter().any(|w| w.contains("compact")), "{why:?}");

    let mut open = matrix(0.5);
    open[1].guards_off = true;
    open[1].guards_fallback = false;
    let Verdict::Stop(why) = bench::evaluate(&open, &accept_all(&open), &[]).verdict else { panic!("guards bypassed with no fallback") };
    assert!(why.iter().any(|w| w.contains("guards")), "{why:?}");
}

#[test]
fn rejected_changes_make_a_run_cost_more_per_accepted_change() {
    let all = matrix(0.6);
    let mut verdicts = accept_all(&all);
    for v in verdicts.iter_mut().filter(|(id, _)| id.contains("claudiu")) {
        v.1 = false;
    }
    let report = bench::evaluate(&all, &verdicts, &[]);
    assert!(report.text.contains("no accepted"), "{}", report.text);
    assert_ne!(report.verdict, Verdict::GoBarMet);
}

#[test]
fn guard_refusals_wait_for_a_human_to_say_whether_they_were_false() {
    let mut all = matrix(0.6);
    all[1].delegations.push(bench::Delegation { profile: "scout".into(), cold_start_tokens: 3000, over_budget: false });
    all[1].refusals.push("Write to C:/elsewhere".into());
    let pending = bench::evaluate(&all, &accept_all(&all), &[]);
    assert!(matches!(pending.verdict, Verdict::Incomplete(_)), "{:?}", pending.verdict);

    let fine = bench::evaluate(&all, &accept_all(&all), &[(all[1].id(), 0, false)]);
    assert_eq!(fine.verdict, Verdict::GoBarMet, "{}", fine.text);
    let bad = bench::evaluate(&all, &accept_all(&all), &[(all[1].id(), 0, true)]);
    assert_eq!(bad.verdict, Verdict::BarNotMet(vec!["false guard refusals".into()]));
}

#[test]
fn the_report_records_pinned_versions_and_each_arms_mode() {
    let all = matrix(0.6);
    let mut report = bench::evaluate(&all, &accept_all(&all), &[]);
    report.text = bench::with_versions(&report.text, &bench::Versions { adapter: "0.94.0".into(), claude: "2.1.306".into(), node: "v22.1.0".into(), core: "0.0.0".into(), os: "windows".into() }, "auto");
    for needle in ["0.94.0", "2.1.306", "v22.1.0", "auto", "stock-haiku", "claudiu-no-firewall"] {
        assert!(report.text.contains(needle), "{needle}:\n{}", report.text);
    }
}
