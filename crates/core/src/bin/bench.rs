//! The spike's measurement. See `docs/BENCH.md`.
//!
//! `bench run --tasks tasks.json --out DIR [--repeats 3] [--arm NAME] [--mode auto]`
//! `bench review --out DIR`   writes the blind review set
//! `bench report --out DIR`   writes DIR/report.md and prints the verdict inputs

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use claudiu_core::bench::{self, Task, Versions};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(flag(&args, "--out").context("--out DIR is required")?);
    match args.first().map(String::as_str) {
        Some("run") => {
            let tasks: Vec<Task> = serde_json::from_str(&std::fs::read_to_string(flag(&args, "--tasks").context("--tasks FILE is required")?)?)?;
            let repeats = flag(&args, "--repeats").map_or(3, |r| r.parse().unwrap_or(3));
            let mode = flag(&args, "--mode").unwrap_or_else(|| "auto".into());
            let (adapter, package, claude) = bench::real_adapter()?;
            std::fs::create_dir_all(&out)?;
            let versions = Versions::detect(&package, &claude);
            std::fs::write(out.join("versions.json"), serde_json::to_string_pretty(&versions)?)?;
            let done = bench::run_all(&adapter, &out, &tasks, repeats, flag(&args, "--arm").as_deref(), &mode)?;
            println!("{} runs in {}", done.len(), out.display());
        }
        Some("review") => {
            bench::prepare_review(&out)?;
            println!("Diffs are in {0}/review. Fill in {0}/review/verdicts.json (accepted: true or false) and refusals.json (false: true or false).", out.display());
        }
        Some("report") => {
            let report = bench::evaluate(&bench::load_results(&out)?, &bench::load_verdicts(&out)?, &bench::load_refusal_reviews(&out));
            let versions: Versions = serde_json::from_str(&std::fs::read_to_string(out.join("versions.json")).context("run `bench run` first")?)?;
            let mode = std::fs::read_to_string(out.join("permission_mode.txt")).unwrap_or_else(|_| "unknown".into());
            let text = bench::with_versions(&report.text, &versions, &mode);
            std::fs::write(out.join("report.md"), &text)?;
            println!("{text}");
        }
        _ => bail!("usage: bench run|review|report --out DIR"),
    }
    Ok(())
}
