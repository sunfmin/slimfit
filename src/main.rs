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
  check             one instrumented test run
  check --mutants   one test run per mutant of the files changed since the
                    baseline's mutants were measured
  check --all-mutants
                    one test run per mutant of every file: hours on a large
                    workspace
  Estimate first: mutant count × one test run (Rust: `cargo mutants --list |
  wc -l`; the time of one run is `test_seconds` plus the build).

MEASUREMENTS DEPEND ON HOST LOAD
  On a busy host the same tests run slower. Do not set or ratchet
  test_seconds then. Busy: the 1-minute load average (`uptime`) is above the
  core count (`sysctl -n hw.ncpu` on macOS, `nproc` on Linux).

BEFORE ADOPTING ON A REPO
  Time `slimfit init` (one instrumented test run) and count the mutants (as
  above). Then place slimfit from those numbers: `slimfit check` in the gate
  in place of the project's own test run; mutants, if they fit at all, in
  idle work, built in pieces on a large workspace (below).

DON'T COMPETE WITH OTHER HEAVY WORK
  Measure on an idle host, or at background priority (`nice -n 19 slimfit
  ...`) so builds, gates and other projects' mutants go first.

THE LOOP
  1. slimfit next             one task: kind, evidence, files, steps, accept command
  2. do exactly that task, nothing else
  3. run the task's accept command (`slimfit check`, `check --mutants` or
     `check --all-mutants`)
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
  uncovered_lines   ≤ baseline + coverage_slack: code may grow, untested code
                    may not. A drop counts as a gain only past the slack, so
                    a line whose coverage flaps by timing never fails the gate
  survived_mutants  ≤ baseline, checked with --mutants or --all-mutants:
                    deleting a test can never lower what the suite catches
  Line counts are recorded and reported, not bounded.

  To raise a bound, run `slimfit ratchet --loosen <bound> --reason \"<why>\"`.
  The reason is kept in the baseline, and every report shows it.

WHAT IS MUTATED
  --all-mutants     every file. Use it for the first baseline (`init --mutants`
                    does), after changing tests outside source files, and to
                    accept a redundant-test task.
  --mutants         only the source files changed since the commit the
                    baseline's mutants were measured at; every other file keeps
                    its count from the baseline. Falls back to every file when
                    the baseline has no such commit or no per-file counts, or
                    the suite's tool cannot be limited to files (only Rust's
                    cargo-mutants can, through `mutants_file_flag`).
                    A test changed outside source files is not seen in the
                    unchanged files' counts: --all-mutants is.
  The output's `mutated` says, per suite, \"all\" or the files mutated.

A LARGE WORKSPACE: BUILD THE MUTANTS BASELINE IN PIECES
  slimfit ratchet --pending-seconds 1500
                    one bounded piece: mutates the files changed since the
                    baseline's mutants were measured (as --mutants does), then
                    files with no count yet, one tool run per file, starting
                    none that would end past the budget (at least one per
                    piece), and ratchets them in. Commit the baseline; repeat
                    until `check --mutants` lists no `mutants_pending`.
                    --pending-files <n> caps the piece at n files instead or
                    as well. A suite with no mutants yet starts with all its
                    source files pending. Until then `check --mutants` holds
                    only the files that have a count; a new file is mutated
                    as a changed one, not left pending.

SET UP
  slimfit init                detect suites (Rust, Go, Swift, Kotlin, TypeScript),
                              write slimfit.toml, measure, write the baseline
  In the gate:                `slimfit check` in place of the project's own test
                              run (e.g. factorium.toml `fast = \"slimfit check\"`
                              with `cargo test` taken out of the gate script)
  Mutants:                    never in `fast` or `slow`; in factorium.toml
                              `[idle] steps = [\"slimfit check --mutants\"]`
                              (runs only while the host is idle). Ratchet the
                              mutants baseline by hand on an idle host: once
                              with --all-mutants, or in pieces, above

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
        /// Also run mutation testing, of every file, for the first baseline (slow).
        #[arg(long)]
        mutants: bool,
    },
    /// The next task to do, with its evidence and accept command.
    Next {
        /// List every task, not just the first (at most 5 of each kind per suite, 3 deepen); `counts` counts them all.
        #[arg(long)]
        all: bool,
        /// Only tasks of this kind.
        #[arg(long, value_enum)]
        kind: Option<tasks::TaskKind>,
        /// Measure mutants too (of the files changed since the baseline): needed
        /// for surviving-mutant tasks.
        #[arg(long)]
        mutants: bool,
        /// Mutate every file: needed for redundant-test tasks.
        #[arg(long)]
        all_mutants: bool,
        /// Re-measure even if the tree is unchanged.
        #[arg(long)]
        fresh: bool,
    },
    /// Fail if any bound is worse than the baseline. Exit 1 on violation.
    Check {
        /// Also check survived_mutants: mutates the files changed since the
        /// baseline's mutants were measured, and keeps the baseline's counts
        /// for the rest.
        #[arg(long)]
        mutants: bool,
        /// Also check survived_mutants, mutating every file.
        #[arg(long)]
        all_mutants: bool,
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
        all_mutants: bool,
        /// Build the mutants baseline in pieces: as --mutants, then also measure
        /// files with no mutant count yet, while the command has run less than
        /// this many seconds (at least one file).
        #[arg(long, conflicts_with = "all_mutants")]
        pending_seconds: Option<u64>,
        /// As --pending-seconds, but at most this many files with no count yet.
        #[arg(long, conflicts_with = "all_mutants", value_parser = clap::value_parser!(u64).range(1..))]
        pending_files: Option<u64>,
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
            let old = ratchet::load(&repo)?;
            let m =
                measure::current(&repo, &cfg, &old, &measure::Options { mutants: scope(false, mutants), fresh: true })?;
            let base = ratchet::Baseline {
                suites: m.suites.iter().map(|s| (s.suite.clone(), s.metrics.clone())).collect(),
                loosened: old.loosened,
                mutants_at: if mutants { m.snapshot.clone() } else { None },
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
            for s in m.suites.iter().filter(|s| s.mutated.is_some()) {
                notes.push(format!(
                    "{}: only the files changed since the baseline were mutated: surviving-mutant tasks name only those; \
                     redundant-test and the other files' survivors need `slimfit next --all-mutants`",
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
        Cmd::Check { mutants, all_mutants, fresh } => {
            let cfg = config::load(&repo)?;
            let base = ratchet::load(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &base,
                &measure::Options { mutants: scope(mutants, all_mutants), fresh },
            )?;
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
        Cmd::Ratchet { loosen, reason, mutants, all_mutants, pending_seconds, pending_files, fresh } => {
            let cfg = config::load(&repo)?;
            let old = ratchet::load(&repo)?;
            let scope = if pending_seconds.is_some() || pending_files.is_some() {
                measure::Mutants::Fill(measure::Budget {
                    files: pending_files.map(|n| n as usize),
                    seconds: pending_seconds,
                })
            } else {
                scope(mutants, all_mutants)
            };
            let m = measure::current(&repo, &cfg, &old, &measure::Options { mutants: scope, fresh })?;
            let new = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            ratchet::save(&repo, &new)?;
            print(json!({
                "baseline": new.suites,
                "loosened": new.loosened,
                "next": "commit .slimfit/baseline.json with the change it records; then `slimfit next` \
                         (while a suite lists mutants_pending, a --pending-seconds piece again)",
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
        (true, false) => measure::Mutants::Changed,
        (false, false) => measure::Mutants::None,
    }
}

/// The metrics, without the per-file mutant counts behind `mutants`.
fn metrics_of(m: &model::Measurement) -> serde_json::Value {
    m.suites
        .iter()
        .map(|s| (s.suite.clone(), json!(model::Metrics { mutant_files: Default::default(), ..s.metrics.clone() })))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// Per suite, what was mutated: "all", or the changed files.
fn mutated_of(m: &model::Measurement) -> serde_json::Value {
    if !m.with_mutants {
        return serde_json::Value::Null;
    }
    m.suites
        .iter()
        .map(|s| (s.suite.clone(), s.mutated.as_ref().map_or(json!("all"), |f| json!(f))))
        .collect::<serde_json::Map<_, _>>()
        .into()
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
