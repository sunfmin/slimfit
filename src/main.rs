mod config;
#[cfg(test)]
mod fixtures;
mod formats;
mod git;
mod load;
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
number: change the code until `slimfit check` passes. What matters most, first:

1. FAST TESTS COME FIRST. slimfit must not slow the feedback loop. A gate runs
   the tests at most once. `slimfit check` IS a test run (instrumented, ~1.5×
   a plain run): it replaces the project's own test run in that gate, never
   adds to it.
2. MUTANTS NEVER IN A GATE. Mutation testing is expensive: never put it in a
   gate that holds up a landing or a promotion. Run it on an idle host at
   background priority (in factorium: `[idle]`).
3. A NEW TEST MUST BE FAST TOO. When goals conflict, cover a line with a fast
   test or delete the code; never with a slow test. This holds for uncovered
   and surviving-mutant tasks alike.

WHAT EACH COMMAND COSTS
  init, check, ratchet
                    one instrumented test run; no mutant runs unless asked
  check --mutants   that, plus one test run per mutant in the lines changed
                    since the baseline's mutants_at (Rust); one per mutant
                    of every file where the tool cannot be limited
  next --all-mutants
                    one test run per mutant of every file: hours on a large
                    workspace
  Estimate first: mutant count × one test run (Rust: `cargo mutants --list
  [--in-diff <diff>] | wc -l`; one run is `test_seconds` plus the build).

MEASUREMENTS DEPEND ON HOST LOAD
  On a busy host everything runs slower. test_seconds is divided by the
  slowdown slimfit measured (`load_slowdown` in the output; ADR 0003), but
  a gate's or a mutants run's wall time is not. Busy: the 1-minute load
  average (`uptime`) is above the core count (`sysctl -n hw.ncpu` on macOS,
  `nproc` on Linux), or `load_slowdown` is well above 1. Then don't set a
  baseline's test_seconds from that run; re-measure when the host is quiet.

BEFORE ADOPTING ON A REPO
  Time `slimfit init` (one instrumented test run; it runs no mutant). Then
  place slimfit from that number: `slimfit check` in the gate in place of the
  project's own test run; `check --mutants` on each change, in idle work.

DON'T COMPETE WITH OTHER HEAVY WORK
  Measure on an idle host, or at background priority (`nice -n 19 slimfit
  ...`) so builds, gates and other projects' mutants go first.

THE LOOP
  1. slimfit next             one task: kind, evidence, files, steps, accept command
  2. do exactly that task, nothing else
  3. run the task's accept command (`slimfit check` or `check --mutants`)
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
                    (sum of the tests' own durations; build time excluded;
                    each divided by how much the host's load slowed a
                    CPU-bound thread meanwhile, so a busy host does not fail
                    it: the measurement's load_slowdown)
  uncovered_lines   ≤ baseline + coverage_slack: code may grow, untested code
                    may not. A drop counts as a gain only past the slack, so
                    a line whose coverage flaps by timing never fails the gate
  survived_mutants  0 in the lines the change added, checked with --mutants:
                    every mutant of new or edited code is killed. Survivors
                    elsewhere are tasks, not violations; no count is kept
  Line counts are recorded and reported, not bounded.

  To raise a bound, run `slimfit ratchet --loosen <bound> --reason \"<why>\"`.
  The reason is kept in the baseline, and every report shows it.

WHAT IS MUTATED (ADR 0005)
  --mutants         the change: the lines that differ from the tree the
                    baseline's mutants_at names (`init` sets it; a ratchet
                    with mutants measured moves it to the tree measured).
                    Rust's cargo-mutants runs only those lines' mutants
                    (`--in-diff`, through `mutants_diff_flag`). A tool with
                    only a file flag runs the touched files; one with neither
                    (every other preset today) runs every mutant. Either way
                    only the survivors in the added lines are held.
                    A change that only deletes code or tests adds no line, so
                    it runs no mutant: what a deleted test killed elsewhere is
                    not seen.
  --all-mutants     (`next` only) every mutant: the whole kill matrix, for
                    redundant-test tasks across the suite.
  The output's `mutated` says, per suite, which files the change touched,
  whether every mutant ran, and the change's mutants by outcome.

SET UP
  slimfit init                detect suites (Rust, Go, Swift, Kotlin, TypeScript),
                              write slimfit.toml, measure, write the baseline
  In the gate:                `slimfit check` in place of the project's own test
                              run (e.g. factorium.toml `fast = \"slimfit check\"`
                              with `cargo test` taken out of the gate script)
  Mutants:                    never in `fast` or `slow`; in factorium.toml
                              `[idle] steps = [\"slimfit check --mutants\"]`
                              (runs only while the host is idle). A commit
                              that changes source carries its mutants: run
                              `check --mutants`, then `ratchet`, and commit
                              the baseline with the change

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
    /// Runs no mutant: mutants are measured on the changes from here on.
    Init,
    /// The next task to do, with its evidence and accept command.
    Next {
        /// List every task, not just the first (at most 5 of each kind per suite, 3 deepen); `counts` counts them all.
        #[arg(long)]
        all: bool,
        /// Only tasks of this kind.
        #[arg(long, value_enum)]
        kind: Option<tasks::TaskKind>,
        /// Measure the mutants of the change too (the lines changed since the
        /// baseline's mutants_at): surviving-mutant and redundant-test tasks
        /// from what they show.
        #[arg(long)]
        mutants: bool,
        /// Measure every mutant: tasks from the whole suite's mutants.
        #[arg(long)]
        all_mutants: bool,
        /// Re-measure even if the tree is unchanged.
        #[arg(long)]
        fresh: bool,
    },
    /// Fail if any bound is worse than the baseline. Exit 1 on violation.
    Check {
        /// Also check survived_mutants: runs the mutants of the lines changed
        /// since the baseline's mutants_at; none may survive.
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
        /// Measure the change's mutants (as `check --mutants`), then move
        /// mutants_at to this tree. A cached `check --mutants` of the same
        /// tree is reused without it.
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
        Cmd::Init => {
            let created = !repo.join(config::FILE).exists();
            if created {
                let suites = config::detect(&git::worktree(&repo)?.files);
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
            let old = ratchet::load(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &old,
                &measure::Options { mutants: measure::Mutants::None, fresh: true },
            )?;
            // Mutants are measured from here on; a mutants_at already kept is not skipped past.
            let mutants_at = match old.mutants_at {
                Some(at) => Some(at),
                None => measure::snapshot(&repo)?,
            };
            let base = ratchet::Baseline {
                suites: m.suites.iter().map(|s| (s.suite.clone(), s.metrics.clone())).collect(),
                loosened: old.loosened,
                mutants_at,
            };
            ratchet::save(&repo, &base)?;
            print(json!({
                "config": if created { "written" } else { "kept" },
                "suites": cfg.suites.iter().map(|s| s.resolve()).collect::<Vec<_>>(),
                "baseline": base.suites,
                "next": "commit slimfit.toml, .slimfit/baseline.json and .gitignore; then `slimfit next`",
            }));
        }
        Cmd::Next { all, kind, mutants, all_mutants, fresh } => {
            let cfg = config::load(&repo)?;
            let base = ratchet::load(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &base,
                &measure::Options { mutants: scope(mutants, all_mutants), fresh },
            )?;
            let mut list = tasks::all(&repo, &cfg, &m)?;
            if let Some(k) = kind {
                list.retain(|t| t.kind == k);
            }
            let mut counts = std::collections::BTreeMap::new();
            for t in &list {
                *counts.entry(serde_json::to_value(t.kind)?.as_str().unwrap_or("").to_string()).or_insert(0) += 1;
            }
            let list = tasks::cap(list);
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
            for s in m.suites.iter().filter(|s| s.mutated.as_ref().is_some_and(|x| !x.all)) {
                notes.push(format!(
                    "{}: only the change's mutants ran: tasks name only what they show (a redundant test only \
                     among them); the whole suite's need `slimfit next --all-mutants`",
                    s.suite
                ));
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
            let base = ratchet::load(&repo)?;
            let m = measure::current(&repo, &cfg, &base, &measure::Options { mutants: scope(mutants, false), fresh })?;
            let v = ratchet::check(&base, &m, &cfg.check);
            let next = if !v.ok {
                "fix each violation as its `fix` says, then `slimfit check` again"
            } else if !v.improved.is_empty() || !v.new_suites.is_empty() {
                "slimfit ratchet, then commit"
            } else {
                "slimfit next"
            };
            let ok = v.ok;
            print(json!({ "verdict": v, "metrics": metrics_of(&m), "mutated": mutated_of(&m), "next": next }));
            if !ok {
                std::process::exit(1);
            }
        }
        Cmd::Ratchet { loosen, reason, mutants, fresh } => {
            let cfg = config::load(&repo)?;
            let old = ratchet::load(&repo)?;
            let m = measure::current(&repo, &cfg, &old, &measure::Options { mutants: scope(mutants, false), fresh })?;
            let new = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            ratchet::save(&repo, &new)?;
            print(json!({
                "baseline": new.suites,
                "loosened": new.loosened,
                "mutants_at": new.mutants_at,
                "next": "commit .slimfit/baseline.json with the change it records; then `slimfit next`",
            }));
        }
        Cmd::Report { commits, points, fresh } => {
            let cfg = config::load(&repo)?;
            let base = ratchet::load(&repo)?;
            let m = measure::current(&repo, &cfg, &base, &measure::Options { mutants: measure::Mutants::None, fresh })?;
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

fn scope(mutants: bool, all_mutants: bool) -> measure::Mutants {
    match (mutants, all_mutants) {
        (_, true) => measure::Mutants::All,
        (true, false) => measure::Mutants::Change,
        (false, false) => measure::Mutants::None,
    }
}

fn metrics_of(m: &model::Measurement) -> serde_json::Value {
    m.suites.iter().map(|s| (s.suite.clone(), json!(s.metrics))).collect::<serde_json::Map<_, _>>().into()
}

/// Per suite, what the mutation run covered and found in the change.
fn mutated_of(m: &model::Measurement) -> serde_json::Value {
    if !m.with_mutants {
        return serde_json::Value::Null;
    }
    m.suites.iter().map(|s| (s.suite.clone(), json!(s.mutated))).collect::<serde_json::Map<_, _>>().into()
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

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    /// What matters most stays on the help's first screen, ahead of the loop:
    /// an agent that reads only that far still keeps its tests fast.
    #[test]
    fn help_leads_with_fast_tests_no_gated_mutants_and_fast_new_tests() {
        let help = super::Cli::command().render_long_help().to_string();
        let lines: Vec<&str> = help.lines().collect();
        let at = |s: &str| lines.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("help lacks {s:?}"));
        let first =
            [at("1. FAST TESTS COME FIRST"), at("2. MUTANTS NEVER IN A GATE"), at("3. A NEW TEST MUST BE FAST TOO")];
        assert!(first.windows(2).all(|w| w[0] < w[1]), "points 1-3 out of order: {first:?}");
        assert!(first[2] + 3 < 24, "point 3 ends past the first screen (line {})", first[2]);
        assert!(first[2] < at("THE LOOP"));
    }
}

#[cfg(test)]
mod gitignore_tests {
    use super::{ensure_gitignore, measure::OUT};

    /// The output directory is ignored once: added after any text (on its own
    /// line), never twice, and an existing `/.../` entry counts.
    #[test]
    fn ensure_gitignore_adds_the_output_dir_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".gitignore");
        let after = |text: Option<&str>| {
            match text {
                Some(t) => std::fs::write(&file, t).unwrap(),
                None => _ = std::fs::remove_file(&file),
            }
            ensure_gitignore(dir.path()).unwrap();
            std::fs::read_to_string(&file).unwrap()
        };
        assert_eq!(after(None), format!("{OUT}\n"));
        assert_eq!(after(Some("target")), format!("target\n{OUT}\n"));
        assert_eq!(after(Some("target\n")), format!("target\n{OUT}\n"));
        for kept in [format!("a\n {OUT} \nb"), format!("/{OUT}/\nb\n"), format!("b\n/{OUT}/")] {
            assert_eq!(after(Some(&kept)), kept);
        }
    }
}
