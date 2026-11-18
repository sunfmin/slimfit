mod config;
mod fault;
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
// build.rs runs it to stamp the version; compiled here too, for its tests and mutants.
#[cfg_attr(not(test), allow(dead_code))]
mod version;

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
  On a busy host everything runs slower. test_seconds is divided by the
  slowdown slimfit measured (`load_slowdown` in the output; ADR 0003), but
  a gate's or a mutants run's wall time is not. Busy: the 1-minute load
  average (`uptime`) is above the core count (`sysctl -n hw.ncpu` on macOS,
  `nproc` on Linux), or `load_slowdown` is well above 1. Then don't set a
  baseline's test_seconds from that run; re-measure when the host is quiet.

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
                    (sum of the tests' own durations; build time excluded;
                    each divided by how much the host's load slowed a
                    CPU-bound thread meanwhile, so a busy host does not fail
                    it: the measurement's load_slowdown)
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
                    A piece is refused only for failing tests, new uncovered
                    lines or more surviving mutants; test_seconds over its
                    bound (a loaded host) is listed in `not_recorded` and
                    the baseline keeps its old test_seconds.

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
  next. Progress goes to stderr. `check` exits 1 when a bound is violated;
  each violation carries its `fix`. Nothing left to do (no task, nothing
  to ratchet) is an answer and exits 0. A command that cannot finish exits
  2 with `{\"error\": [{\"fault\": ..., \"fix\": ...}, ...]}`: every fault it
  found, each with the command or edit that fixes it. Do every fix, then run
  the command again.
  Measurements are cached per working tree in .slimfit/out/; --fresh re-runs.";

#[derive(Parser)]
#[command(name = "slimfit", version = env!("SLIMFIT_VERSION"), about = ABOUT, long_about = LONG_ABOUT, arg_required_else_help = true)]
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

fn main() {
    let (answer, code) = match run(std::env::args_os()) {
        Ok(done) => done,
        Err(e) => (json!({ "error": fault::of(e) }), 2),
    };
    println!("{}", serde_json::to_string_pretty(&answer).expect("json"));
    std::process::exit(code);
}

/// The command line's answer and exit code (help and version print and exit
/// here, as clap does).
fn run(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<(serde_json::Value, i32)> {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) if !e.use_stderr() || e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            e.exit()
        }
        Err(e) => {
            let text = e.render().to_string();
            let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("For more"));
            let first = lines.next().unwrap_or_default().trim_start_matches("error: ").to_string();
            let hints: Vec<&str> = lines.collect();
            return Err(fault::fault(
                first,
                format!(
                    "correct the command line{}; `slimfit --help` is the manual, `slimfit <command> --help` lists a \
                     command's flags",
                    hints.iter().map(|h| format!(" ({h})")).collect::<String>()
                ),
            ));
        }
    };
    let start = cli.repo.clone().unwrap_or(std::env::current_dir()?);
    let repo = git::toplevel(&start)?;
    match cli.command {
        Cmd::Init { mutants } => {
            let created = !repo.join(config::FILE).exists();
            if created {
                let suites = config::detect(&git::worktree(&repo)?.files);
                if suites.is_empty() {
                    return Err(fault::fault(
                        format!("no Rust, Go, Swift, Kotlin or TypeScript project found in {}", repo.display()),
                        format!(
                            "write {} by hand: a `[[suite]]` with name, preset (the language nearest yours) and \
                             root, overriding test_command, tests_report and coverage_report; then `slimfit init`",
                            config::FILE
                        ),
                    ));
                }
                let cfg = config::Config { suites, check: Default::default() };
                std::fs::write(repo.join(config::FILE), toml::to_string_pretty(&cfg)?)?;
            }
            ensure_gitignore(&repo)?;
            let (cfg, old) = loaded(&repo)?;
            let m =
                measure::current(&repo, &cfg, &old, &measure::Options { mutants: scope(false, mutants), fresh: true })?;
            let base = ratchet::Baseline {
                suites: m.suites.iter().map(|s| (s.suite.clone(), s.metrics.clone())).collect(),
                loosened: old.loosened,
                mutants_at: if mutants { m.snapshot.clone() } else { None },
            };
            ratchet::save(&repo, &base)?;
            Ok((
                json!({
                    "config": if created { "written" } else { "kept" },
                    "suites": cfg.suites.iter().map(|s| s.resolve()).collect::<Vec<_>>(),
                    "baseline": base.suites,
                    "next": "commit slimfit.toml, .slimfit/baseline.json and .gitignore; then `slimfit next`",
                }),
                0,
            ))
        }
        Cmd::Next { all, kind, mutants, all_mutants, fresh } => {
            let (cfg, base) = loaded(&repo)?;
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
            Ok((
                if all {
                    json!({ "tasks": list, "counts": counts, "notes": notes, "next": next })
                } else {
                    json!({ "task": list.first(), "remaining": counts, "notes": notes, "next": next })
                },
                0,
            ))
        }
        Cmd::Check { mutants, all_mutants, fresh } => {
            let (cfg, base) = loaded(&repo)?;
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
            let code = if v.ok { 0 } else { 1 };
            Ok((json!({ "verdict": v, "metrics": metrics_of(&m), "mutated": mutated_of(&m), "next": next }), code))
        }
        Cmd::Ratchet { loosen, reason, mutants, all_mutants, pending_seconds, pending_files, fresh } => {
            let (cfg, old) = loaded(&repo)?;
            let scope = if pending_seconds.is_some() || pending_files.is_some() {
                measure::Mutants::Fill(measure::Budget {
                    files: pending_files.map(|n| n as usize),
                    seconds: pending_seconds,
                })
            } else {
                scope(mutants, all_mutants)
            };
            let m = measure::current(&repo, &cfg, &old, &measure::Options { mutants: scope, fresh })?;
            let (new, not_recorded) = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            // Nothing to ratchet is an answer, not a failure.
            let changed = new != old;
            if changed {
                ratchet::save(&repo, &new)?;
            }
            let next = if changed {
                "commit .slimfit/baseline.json with the change it records; then `slimfit next` \
                 (while a suite lists mutants_pending, a --pending-seconds piece again)"
            } else {
                "nothing to ratchet: the baseline already holds this measurement; `slimfit next`"
            };
            Ok((
                json!({
                    "changed": changed,
                    "baseline": new.suites,
                    "loosened": new.loosened,
                    "not_recorded": not_recorded,
                    "next": next,
                }),
                0,
            ))
        }
        Cmd::Report { commits, points, fresh } => {
            let (cfg, base) = loaded(&repo)?;
            let m = measure::current(&repo, &cfg, &base, &measure::Options { mutants: measure::Mutants::None, fresh })?;
            let history = report::history(&repo, &cfg, commits, points)?;
            Ok((
                json!({
                    "current": metrics_of(&m),
                    "baseline": base.suites,
                    "loosened": base.loosened,
                    "history": history,
                    "next": "slimfit next",
                }),
                0,
            ))
        }
    }
}

/// slimfit.toml and the baseline, or the faults of both.
fn loaded(repo: &std::path::Path) -> Result<(config::Config, ratchet::Baseline)> {
    match (config::load(repo), ratchet::load(repo)) {
        (Ok(c), Ok(b)) => Ok((c, b)),
        (c, b) => Err(fault::Faults(c.err().into_iter().chain(b.err()).flat_map(fault::of).collect()).into()),
    }
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

#[cfg(test)]
mod answer_tests {
    use super::{fault::Fault, run};
    use crate::measure::t::{git, sample_repo};
    use serde_json::Value;
    use std::path::Path;

    fn slimfit(repo: &Path, args: &str) -> anyhow::Result<(Value, i32)> {
        let mut all: Vec<std::ffi::OsString> = vec!["slimfit".into(), "--repo".into(), repo.into()];
        all.extend(args.split_whitespace().map(Into::into));
        run(all)
    }

    fn faults(repo: &Path, args: &str) -> Vec<Fault> {
        crate::fault::of(slimfit(repo, args).unwrap_err())
    }

    /// Each fault of a command line, a place and a repo carries its fix;
    /// slimfit.toml's and the baseline's are named together.
    #[test]
    fn a_failure_names_every_fault_with_its_fix() {
        let gone = faults(Path::new("/nonexistent"), "next");
        assert_eq!(gone[0].fault, "/nonexistent is not a directory");
        assert!(gone[0].fix.starts_with("run slimfit inside the repository, or pass `--repo <its path>`"));
        let dir = tempfile::tempdir().unwrap();
        let f = faults(dir.path(), "next");
        assert!(f[0].fault.contains("is not inside a git repository") && f[0].fix == gone[0].fix, "{f:?}");
        let f = faults(dir.path(), "nxt");
        assert_eq!(f[0].fault, "unrecognized subcommand 'nxt'");
        assert!(f[0].fix.starts_with("correct the command line (tip: some similar subcommands exist: 'init', 'next')"));

        git(dir.path(), &["init", "-q"]);
        let f = faults(dir.path(), "init");
        assert!(f[0].fault.starts_with("no Rust, Go, Swift, Kotlin or TypeScript project found"), "{f:?}");
        assert!(f[0].fix.starts_with("write slimfit.toml by hand: a `[[suite]]` with name, preset"), "{f:?}");
        std::fs::create_dir(dir.path().join(".slimfit")).unwrap();
        std::fs::write(dir.path().join(".slimfit/baseline.json"), "[]").unwrap();
        let f: Vec<String> = faults(dir.path(), "check").into_iter().map(|f| f.fix).collect();
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(f[0].starts_with("`slimfit init` writes it") && f[1].starts_with("restore it: `git checkout"));
    }

    /// No task left of a kind, and nothing to ratchet, are answers: exit 0.
    #[test]
    fn nothing_to_do_is_an_answer() {
        let (dir, cfg) = sample_repo();
        std::fs::write(dir.path().join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        let (first, code) = slimfit(dir.path(), "ratchet").unwrap();
        assert_eq!((first["changed"].as_bool(), code), (Some(true), 0));
        let (again, code) = slimfit(dir.path(), "ratchet").unwrap();
        assert_eq!((again["changed"].as_bool(), code), (Some(false), 0));
        assert!(again["next"].as_str().unwrap().starts_with("nothing to ratchet"));
        let (all, _) = slimfit(dir.path(), "next --all").unwrap();
        assert_eq!(all["counts"], serde_json::json!({ "slow-test": 1 }));
        assert!(all["notes"][0].as_str().unwrap().starts_with("mutants not measured"), "{all}");
        let (held, code) = slimfit(dir.path(), "check").unwrap();
        assert_eq!((held["next"].as_str(), code), (Some("slimfit next"), 0));
        let (none, code) = slimfit(dir.path(), "next --kind deepen").unwrap();
        assert_eq!((none["task"].clone(), code), (Value::Null, 0));
    }
}
