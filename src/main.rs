mod config;
#[cfg(test)]
mod fixtures;
mod formats;
mod git;
mod loc;
mod measure;
mod model;
mod ratchet;
mod report;
mod tasks;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;

const ABOUT: &str = "Keep a codebase small and its tests few, fast and strong.";

const LONG_ABOUT: &str = "\
Keep a codebase small and its tests few, fast and strong.

You (the agent) change code; slimfit measures and ratchets. Never argue with a
number: change the code until `slimfit check` passes.

THE LOOP
  1. slimfit next             one task: kind, evidence, files, steps, accept command
  2. do exactly that task, nothing else
  3. run the task's accept command (`slimfit check`, or `slimfit check --mutants`)
  4. slimfit ratchet          lock the gain into .slimfit/baseline.json
  5. commit code and baseline together (one task = one commit); back to 1

TASK KINDS (`slimfit next` gives them in this order: cheapest and safest first)
  failing-test      a test fails. Fix it first; nothing else is measured honestly.
  uncovered         lines no test runs. No caller: delete. A user path needs it:
                    test it through its module's interface. A feature nobody
                    uses: don't delete, escalate: that is a product decision.
  redundant-test    every mutant it kills, a cheaper set of tests kills too.
                    Delete it unless it pins behaviour mutants don't reach.
  slow-test         one test holds a large share of the suite's time. Make it
                    cheaper without testing less, or merge it into faster ones.
  surviving-mutant  a change to the code no test notices. Assert the behaviour
                    through the interface, or simplify the code away.
  deepen            a big file that keeps changing. Load the codebase-design
                    and improve-codebase-architecture skills; design the deeper
                    interface first, then move to it one passing step at a time.

WHAT `slimfit check` HOLDS (against .slimfit/baseline.json)
  failures          0
  test_seconds      ≤ baseline × (1 + time_tolerance) + time_slack
                    (sum of the tests' own durations; build time excluded)
  uncovered_lines   ≤ baseline: code may grow, untested code may not
  survived_mutants  ≤ baseline, checked with --mutants: deleting a test can
                    never lower what the suite catches
  Line counts are recorded and reported, not bounded.

  To raise a bound, run `slimfit ratchet --loosen <bound> --reason \"<why>\"`.
  The reason is kept in the baseline, and every report shows it.

SET UP
  slimfit init                detect suites (Rust, Go, Swift, Kotlin, TypeScript),
                              write slimfit.toml, measure, write the baseline
  In factorium.toml:          fast = \"slimfit check\", slow = \"slimfit check --mutants\"

OUTPUT
  Every command prints JSON on stdout with a `next` field: the command to run
  next. Progress goes to stderr. `check` exits 1 when a bound is violated.
  Measurements are cached per working tree in .slimfit/out/; --fresh re-runs.";

#[derive(Parser)]
#[command(name = "slimfit", version, about = ABOUT, long_about = LONG_ABOUT, arg_required_else_help = true)]
struct Cli {
    /// Repository to work on (default: the one containing the current directory).
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect suites, write slimfit.toml (if missing), measure, write the baseline.
    Init {
        /// Also run mutation testing for the first baseline (slow).
        #[arg(long)]
        mutants: bool,
    },
    /// The next task to do, with its evidence and accept command.
    Next {
        /// Every task, not just the first.
        #[arg(long)]
        all: bool,
        /// Only tasks of this kind.
        #[arg(long, value_enum)]
        kind: Option<tasks::TaskKind>,
        /// Measure mutants too: needed for redundant-test and surviving-mutant tasks.
        #[arg(long)]
        mutants: bool,
        /// Re-measure even if the tree is unchanged.
        #[arg(long)]
        fresh: bool,
    },
    /// Fail if any bound is worse than the baseline. Exit 1 on violation.
    Check {
        /// Also check survived_mutants (runs mutation testing).
        #[arg(long)]
        mutants: bool,
        #[arg(long)]
        fresh: bool,
    },
    /// Move the baseline to the current measurement; bounds only tighten.
    Ratchet {
        /// Allow this bound to rise (repeatable); needs --reason.
        #[arg(long, value_enum)]
        loosen: Vec<ratchet::Bound>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        mutants: bool,
        #[arg(long)]
        fresh: bool,
    },
    /// Current metrics against the baseline, and line counts over git history.
    Report {
        /// How many commits back to sample line counts from.
        #[arg(long, default_value_t = 200)]
        commits: usize,
        /// How many points to sample in that range.
        #[arg(long, default_value_t = 10)]
        points: usize,
        #[arg(long)]
        fresh: bool,
    },
}

fn print(v: serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(&v).expect("json"));
}

fn main() {
    if let Err(e) = run() {
        print(json!({ "error": format!("{e:#}") }));
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let start = cli.repo.clone().unwrap_or(std::env::current_dir()?);
    let repo = git::toplevel(&start)?;
    match cli.command {
        Cmd::Init { mutants } => {
            let created = !repo.join(config::FILE).exists();
            if created {
                let suites = config::detect(&git::files(&repo)?);
                if suites.is_empty() {
                    anyhow::bail!(
                        "no Rust, Go, Swift, Kotlin or TypeScript project found; write {} by hand",
                        config::FILE
                    );
                }
                let cfg = config::Config { suites, check: Default::default() };
                std::fs::write(repo.join(config::FILE), toml::to_string_pretty(&cfg)?)?;
            }
            ensure_gitignore(&repo)?;
            let cfg = config::load(&repo)?;
            let m = measure::current(&repo, &cfg, &measure::Options { mutants, fresh: true })?;
            let base = ratchet::Baseline {
                suites: m.suites.iter().map(|s| (s.suite.clone(), s.metrics.clone())).collect(),
                loosened: ratchet::load(&repo)?.loosened,
            };
            ratchet::save(&repo, &base)?;
            print(json!({
                "config": if created { "written" } else { "kept" },
                "suites": cfg.suites.iter().map(|s| s.resolve()).collect::<Vec<_>>(),
                "baseline": base.suites,
                "next": "commit slimfit.toml, .slimfit/baseline.json and .gitignore; then `slimfit next`",
            }));
        }
        Cmd::Next { all, kind, mutants, fresh } => {
            let cfg = config::load(&repo)?;
            let m = measure::current(&repo, &cfg, &measure::Options { mutants, fresh })?;
            let mut list = tasks::all(&repo, &cfg, &m)?;
            if let Some(k) = kind {
                list.retain(|t| t.kind == k);
            }
            let mut counts = std::collections::BTreeMap::new();
            for t in &list {
                *counts.entry(serde_json::to_value(t.kind)?.as_str().unwrap_or("").to_string()).or_insert(0) += 1;
            }
            let mut notes = Vec::new();
            if !m.with_mutants {
                notes.push(
                    "mutants not measured: redundant-test and surviving-mutant tasks need `slimfit next --mutants`"
                        .to_string(),
                );
            }
            for s in m.suites.iter().filter(|s| {
                m.with_mutants && s.mutation_tests.is_none() && s.mutants.iter().all(|m| m.killed_by.is_none())
            }) {
                notes.push(format!("{}: the mutation tool does not record which test kills which mutant, so redundant tests cannot be found", s.suite));
            }
            let next = if list.is_empty() {
                if m.with_mutants {
                    "nothing left to do: the codebase is at its fittest by these measures"
                } else {
                    "slimfit next --mutants"
                }
            } else {
                "do the task, then run its accept command, then `slimfit ratchet` and commit"
            };
            if all {
                print(json!({ "tasks": list, "counts": counts, "notes": notes, "next": next }));
            } else {
                print(json!({ "task": list.first(), "remaining": counts, "notes": notes, "next": next }));
            }
        }
        Cmd::Check { mutants, fresh } => {
            let cfg = config::load(&repo)?;
            let m = measure::current(&repo, &cfg, &measure::Options { mutants, fresh })?;
            let base = ratchet::load(&repo)?;
            let v = ratchet::check(&base, &m, &cfg.check);
            let next = if !v.ok {
                "fix each violation as its `fix` says, then `slimfit check` again"
            } else if !v.improved.is_empty() || !v.new_suites.is_empty() {
                "slimfit ratchet, then commit"
            } else {
                "slimfit next"
            };
            let ok = v.ok;
            print(json!({ "verdict": v, "metrics": metrics_of(&m), "next": next }));
            if !ok {
                std::process::exit(1);
            }
        }
        Cmd::Ratchet { loosen, reason, mutants, fresh } => {
            let cfg = config::load(&repo)?;
            let m = measure::current(&repo, &cfg, &measure::Options { mutants, fresh })?;
            let old = ratchet::load(&repo)?;
            let new = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            ratchet::save(&repo, &new)?;
            print(
                json!({ "baseline": new.suites, "loosened": new.loosened, "next": "commit .slimfit/baseline.json with the change it records; then `slimfit next`" }),
            );
        }
        Cmd::Report { commits, points, fresh } => {
            let cfg = config::load(&repo)?;
            let m = measure::current(&repo, &cfg, &measure::Options { mutants: false, fresh })?;
            let base = ratchet::load(&repo)?;
            let history = report::history(&repo, &cfg, commits, points)?;
            print(json!({
                "current": metrics_of(&m),
                "baseline": base.suites,
                "loosened": base.loosened,
                "history": history,
                "next": "slimfit next",
            }));
        }
    }
    Ok(())
}

fn metrics_of(m: &model::Measurement) -> serde_json::Value {
    m.suites.iter().map(|s| (s.suite.clone(), json!(s.metrics))).collect::<serde_json::Map<_, _>>().into()
}

fn ensure_gitignore(repo: &std::path::Path) -> Result<()> {
    let path = repo.join(".gitignore");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.lines().any(|l| l.trim() == measure::OUT || l.trim() == format!("/{}/", measure::OUT)) {
        let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
        std::fs::write(&path, format!("{text}{sep}{}\n", measure::OUT))?;
    }
    Ok(())
}
