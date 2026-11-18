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
mod tools;
// build.rs runs it to stamp the version; compiled here too, for its tests and mutants.
#[cfg_attr(not(test), allow(dead_code))]
mod version;

use agent_help::{Help, Topic};
use anyhow::Result;
use clap::{FromArgMatches, Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;

agent_help::binary!("slimfit");

/// Keeps a codebase small and its tests few, fast and strong.
///
/// You (the agent) change code; slimfit measures each change and ratchets the result. Never argue with a number: change the code until `slimfit check` passes. Run `slimfit <command> --help` before a command's first run. What matters most, first:
///
/// 1. FAST TESTS COME FIRST. slimfit must not slow the feedback loop. A gate runs the tests at most once. `slimfit check` IS a test run (instrumented, about 1.5× a plain run): it replaces the project's own test run in that gate, never adds to it.
/// 2. MUTANTS NEVER IN A GATE. Mutation testing is expensive: never put it in a gate that holds up a landing or a promotion. Run it on an idle host at background priority (in factorium: `[idle]`, see [setting up in factorium](help:factorium)).
/// 3. A NEW TEST MUST BE FAST TOO. When goals conflict, cover a line with a fast test or delete the code; never with a slow test. This holds for uncovered and surviving-mutant tasks alike.
///
/// ## The loop
///
/// 1. `slimfit next`: one task, with its kind, evidence, files, steps and accept command.
/// 2. Do exactly that task, nothing else.
/// 3. Run the task's accept command (`slimfit check`, `slimfit check --mutants` or `slimfit check --all-mutants`).
/// 4. `slimfit ratchet`: lock the gain into `.slimfit/baseline.json`.
/// 5. Commit the code and the baseline together (one task, one commit); back to 1.
///
/// Before adopting slimfit on a repo, read [costs](help:costs) and [setting up in factorium](help:factorium).
///
/// ## Output
///
/// Every command prints one JSON object on stdout with a `next` field: the command to run next. Progress goes to stderr. A failure prints `{"error": "..."}` and exits 2; `slimfit check` exits 1 when a bound is violated. Measurements are cached per working tree in `.slimfit/out/`; `--fresh` measures again.
#[derive(Parser)]
#[command(
    name = "slimfit",
    version = env!("SLIMFIT_VERSION"),
    verbatim_doc_comment,
    disable_help_subcommand = true,
    arg_required_else_help = true
)]
struct Cli {
    /// The repository to work on (default: the one containing the current directory).
    #[arg(long, global = true, verbatim_doc_comment)]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detects the suites, writes `slimfit.toml` (if missing), measures, and writes the baseline.
    ///
    /// Run it once, when adopting slimfit on a repo; time it first (see [costs](help:costs)). It detects Rust, Go, Swift, Kotlin and TypeScript projects, one suite per project found, each from its language's preset; edit `slimfit.toml` to override any preset field.
    ///
    /// ## What it reads and writes
    ///
    /// It writes `slimfit.toml` when the repo has none, and keeps it otherwise. It measures every suite once (one instrumented test run, never from the cache) and writes `.slimfit/baseline.json` from that measurement, keeping any loosened bounds already there. It adds `.slimfit/out` to `.gitignore`.
    ///
    /// ## What it answers
    ///
    /// - `config`: `"written"` or `"kept"`.
    /// - `suites`: each suite's settings, the preset filled in.
    /// - `baseline`: per suite, the metrics now held (see `slimfit check --help`).
    /// - `next`: commit `slimfit.toml`, `.slimfit/baseline.json` and `.gitignore`, then `slimfit next`.
    ///
    /// It refuses when it finds no project of a language it knows: write `slimfit.toml` by hand.
    #[command(next_help_heading = "Run once, to adopt slimfit on a repo", verbatim_doc_comment)]
    Init {
        /// Also run mutation testing of every file, for the first mutants baseline: hours on a large workspace (see [the piecewise baseline](help:piecewise-baseline)).
        #[arg(long, verbatim_doc_comment)]
        mutants: bool,
    },
    /// The next task to do, with its evidence and accept command.
    ///
    /// Run it at the start of each turn of the loop. Tasks come cheapest and safest first, in the order of `--kind`'s values below. Each task carries its evidence and the command that accepts it; do exactly that task, then run its accept command, then `slimfit ratchet` and commit.
    ///
    /// ## What it answers
    ///
    /// - `task`: the first task, or `null` when none is left. With `--all`, `tasks` lists them instead.
    /// - `remaining` (`counts` with `--all`): how many tasks of each kind there are, past the listed ones too.
    /// - `notes`: what was not measured and what that hides, e.g. mutants not run, or a mutation tool that does not record which test kills which mutant (only Rust, TypeScript and Kotlin record it, so only they get redundant-test tasks).
    /// - `next`: what to do now.
    ///
    /// A task has `kind`, `suite`, `summary`, `files`, `evidence` (the numbers that make it a task), `steps` (what to do) and `accept` (the command that must pass after).
    ///
    /// ## What trips you up
    ///
    /// Without `--mutants` there are no surviving-mutant tasks, and without `--all-mutants` no redundant-test tasks: `notes` says so. Those flags cost what [costs](help:costs) says.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Next {
        /// List every task, not just the first (at most {const:LISTED_PER_KIND} of each kind per suite, {const:LISTED_DEEPEN} deepen); `counts` counts them all.
        #[arg(long, verbatim_doc_comment)]
        all: bool,
        /// Only tasks of this kind.
        #[arg(long, value_enum, verbatim_doc_comment)]
        kind: Option<tasks::TaskKind>,
        /// Measure mutants too, of the files changed since the baseline: needed for surviving-mutant tasks (see [mutants scope](help:mutants-scope)).
        #[arg(long, verbatim_doc_comment)]
        mutants: bool,
        /// Measure mutants of every file: needed for redundant-test tasks.
        #[arg(long, verbatim_doc_comment)]
        all_mutants: bool,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Fails if any bound is worse than the baseline; exits 1 on a violation.
    ///
    /// Run it after each change, and in the gate in place of the project's own test run: it IS that test run (see [setting up in factorium](help:factorium)). It reads `slimfit.toml` and `.slimfit/baseline.json` and writes nothing but its cache in `.slimfit/out/`.
    ///
    /// ## What it holds
    ///
    /// Per suite, against `.slimfit/baseline.json`:
    ///
    /// - `failures`: 0.
    /// - `test_seconds`: at most baseline × (1 + `time_tolerance`) + `time_slack` (`slimfit.toml`'s `[check]`; by default {const:TIME_TOLERANCE} and {const:TIME_SLACK} seconds). It is the sum of the tests' own durations, build time excluded, corrected for the host's load (see [host load](help:host-load)).
    /// - `uncovered_lines`: at most baseline + `coverage_slack` (by default {const:COVERAGE_SLACK}): code may grow, untested code may not. A drop counts as a gain only past the slack, so a line whose coverage flaps by timing never fails the gate.
    /// - `survived_mutants`: at most the baseline, checked only with `--mutants` or `--all-mutants`: deleting a test can never lower what the suite catches.
    ///
    /// Line counts are recorded and reported, not bounded. To raise a bound, see `slimfit ratchet --help`.
    ///
    /// ## What it answers
    ///
    /// - `verdict`: `ok`; `violations`, each with `suite`, `metric`, `baseline`, `limit`, `current` and `fix` (what to do); `improved`, the bounds that got better (`slimfit ratchet` locks them in); `new_suites`, suites with no baseline yet; `mutants_unchecked`, true when mutants were not measured; and, while a mutants baseline is built in pieces, `mutants_pending`, per suite the files whose mutants are not held yet.
    /// - `metrics`: per suite, `prod_lines`, `test_lines`, `tests`, `failures`, `test_seconds`, `covered_lines`, `uncovered_lines` and, when measured, `mutants` (`killed`, `survived`, `unviable`).
    /// - `mutated`: per suite, `"all"` or the files mutated; `null` without mutants (see [mutants scope](help:mutants-scope)).
    /// - `next`: what to do now.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Check {
        /// Also check `survived_mutants`, mutating the files changed since the baseline's mutants were measured and keeping the baseline's counts for the rest (see [mutants scope](help:mutants-scope)). Never in a gate.
        #[arg(long, verbatim_doc_comment)]
        mutants: bool,
        /// Also check `survived_mutants`, mutating every file. Never in a gate.
        #[arg(long, verbatim_doc_comment)]
        all_mutants: bool,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Moves the baseline to the current measurement; bounds only tighten.
    ///
    /// Run it after a task's accept command passes, then commit `.slimfit/baseline.json` with the change it records. It refuses, writing nothing, when a bound got worse, unless `--loosen` allows that bound to rise; `failures` never may. A loosening's reason is kept in the baseline, and every report shows it.
    ///
    /// With `--mutants` or `--all-mutants` it also records the commit the mutants were measured at, so the next `--mutants` mutates only what changed after it. `--pending-seconds` and `--pending-files` build the mutants baseline in pieces (see [the piecewise baseline](help:piecewise-baseline)).
    ///
    /// ## What it answers
    ///
    /// - `baseline`: per suite, the metrics now held.
    /// - `loosened`: every loosening kept in the baseline, with its reason.
    /// - `not_recorded`: in a piece, a `test_seconds` over its bound, left at its old value (see [host load](help:host-load)).
    /// - `next`: commit, then `slimfit next`.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Ratchet {
        /// Allow this bound to rise (repeatable); needs `--reason`.
        #[arg(long, value_enum, verbatim_doc_comment)]
        loosen: Vec<ratchet::Bound>,
        /// Why the bound may rise, kept in the baseline.
        #[arg(long, verbatim_doc_comment)]
        reason: Option<String>,
        /// Measure mutants of the files changed since the baseline's mutants were measured, and record them.
        #[arg(long, verbatim_doc_comment)]
        mutants: bool,
        /// Measure mutants of every file, and record them.
        #[arg(long, verbatim_doc_comment)]
        all_mutants: bool,
        /// One piece of the mutants baseline: as `--mutants`, then also files with no mutant count yet, while the command has run less than this many seconds (at least one file).
        #[arg(long, conflicts_with = "all_mutants", verbatim_doc_comment)]
        pending_seconds: Option<u64>,
        /// As `--pending-seconds`, but at most this many files with no count yet.
        #[arg(long, conflicts_with = "all_mutants", value_parser = clap::value_parser!(u64).range(1..), verbatim_doc_comment)]
        pending_files: Option<u64>,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Current metrics against the baseline, and line counts over git history.
    ///
    /// Run it to see where the codebase stands and how its size moved. It measures as `slimfit check` does (from the cache when the tree is unchanged) and reads line counts at commits spread over the history.
    ///
    /// ## What it answers
    ///
    /// - `current`: per suite, the metrics measured now (see `slimfit check --help`).
    /// - `baseline`: per suite, the metrics held.
    /// - `loosened`: every loosening kept in the baseline, with its reason.
    /// - `history`: oldest first, each point's `commit`, `date` and per suite `prod` and `test` lines.
    /// - `next`: `slimfit next`.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Report {
        /// How many first-parent commits back to sample line counts from.
        #[arg(long, default_value_t = 200, verbatim_doc_comment)]
        commits: usize,
        /// How many points to sample in that range.
        #[arg(long, default_value_t = 10, verbatim_doc_comment)]
        points: usize,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Shows one topic of this manual, or all of it.
    ///
    /// `slimfit help costs` shows what each command costs; `slimfit help all` shows the whole manual, every command and topic in full. Without a topic it shows what `slimfit --help` does.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Help {
        /// The topic, such as `costs`, or `all`.
        #[arg(verbatim_doc_comment)]
        topic: Option<String>,
    },
}

#[doc = include_str!("help/costs.md")]
#[derive(Topic)]
#[topic(name = "costs")]
struct Costs;

#[doc = include_str!("help/host-load.md")]
#[derive(Topic)]
#[topic(name = "host-load")]
struct HostLoad;

#[doc = include_str!("help/mutants-scope.md")]
#[derive(Topic)]
#[topic(name = "mutants-scope")]
struct MutantsScope;

#[doc = include_str!("help/piecewise-baseline.md")]
#[derive(Topic)]
#[topic(name = "piecewise-baseline")]
struct PiecewiseBaseline;

#[doc = include_str!("help/factorium.md")]
#[derive(Topic)]
#[topic(name = "factorium")]
struct Factorium;

/// The manual: `slimfit --help` answers its index, `slimfit <command> --help` a command, `slimfit help <topic>` a topic.
fn help() -> Help {
    let check = config::CheckConfig::default();
    Help::new(<Cli as clap::CommandFactory>::command())
        .topic::<Costs>()
        .topic::<HostLoad>()
        .topic::<MutantsScope>()
        .topic::<PiecewiseBaseline>()
        .topic::<Factorium>()
        .constant("TIME_TOLERANCE", check.time_tolerance)
        .constant("TIME_SLACK", check.time_slack)
        .constant("COVERAGE_SLACK", check.coverage_slack)
        .constant("LISTED_PER_KIND", tasks::LISTED_PER_KIND)
        .constant("LISTED_DEEPEN", tasks::LISTED_DEEPEN)
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
    // `--help`, `-h` and `help` too: clap prints the manual's answer, on stdout.
    let cli = parse(&help(), std::env::args_os()).unwrap_or_else(|e| e.exit());
    let start = cli.repo.clone().unwrap_or(std::env::current_dir()?);
    let repo = git::toplevel(&start)?;
    match cli.command {
        Cmd::Init { mutants } => {
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
            let (new, not_recorded) = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            ratchet::save(&repo, &new)?;
            print(json!({
                "baseline": new.suites,
                "loosened": new.loosened,
                "not_recorded": not_recorded,
                "next": "commit .slimfit/baseline.json with the change it records; then `slimfit next` \
                         (while a suite lists mutants_pending, a --pending-seconds piece again)",
            }));
        }
        Cmd::Help { .. } => unreachable!("`parse` answers it"),
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

/// The command line, read against the manual. What `--help`, `-h` and
/// `help [<topic>]` answer comes back as the error clap prints on stdout,
/// exiting 0; a topic that does not exist, as one it prints on stderr,
/// exiting 2, as for any other bad argument.
fn parse(help: &Help, args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Cli, clap::Error> {
    let cli = Cli::from_arg_matches(&help.clap().expect("checked by the_manual_is_whole").try_get_matches_from(args)?)?;
    let Cmd::Help { topic } = &cli.command else { return Ok(cli) };
    let answer = topic.as_deref().map_or_else(|| help.index(), |t| help.topic_help(t));
    Err(match answer {
        // clap's own `--help` answer, so it prints as `slimfit --help` does.
        Ok(text) => {
            clap::Command::new("slimfit").override_help(text).try_get_matches_from(["slimfit", "--help"]).unwrap_err()
        }
        Err(e) => clap::Error::raw(clap::error::ErrorKind::InvalidValue, format!("{e}\n")),
    })
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
    /// The one test that keeps the manual whole: every command has text and a
    /// group, every placeholder and topic link names something.
    #[test]
    fn the_manual_is_whole() {
        agent_help::check(&super::help()).unwrap();
    }

    /// `--help`, `-h` and `help` answer the index, `<command> --help` that
    /// command, `help <topic>` that topic and `help all` the whole manual; a
    /// topic that does not exist is a bad argument.
    #[test]
    fn help_answers_from_the_manual() {
        use clap::error::ErrorKind;
        let help = super::help();
        let answer = |args: &str| {
            let e = super::parse(&help, args.split(' ').map(Into::into)).err().expect("an answer, not a command");
            (e.kind(), e.render().to_string())
        };
        let index = help.index().unwrap();
        for args in ["slimfit --help", "slimfit -h", "slimfit help", "slimfit --repo x help"] {
            assert_eq!(answer(args), (ErrorKind::DisplayHelp, index.clone()), "{args}");
        }
        assert_eq!(answer("slimfit check --help"), (ErrorKind::DisplayHelp, help.command_help(&["check"]).unwrap()));
        assert_eq!(answer("slimfit help costs"), (ErrorKind::DisplayHelp, help.topic_help("costs").unwrap()));
        assert_eq!(answer("slimfit help all"), (ErrorKind::DisplayHelp, help.manual().unwrap()));
        let (kind, text) = answer("slimfit help nope");
        assert_eq!(kind, ErrorKind::InvalidValue);
        assert!(text.contains("costs"), "{text}");
        assert!(super::parse(&help, ["slimfit", "check"].map(Into::into)).is_ok());
    }

    /// What matters most stays on the index's first screen, ahead of the
    /// loop and the commands: an agent that reads only that far still keeps
    /// its tests fast.
    #[test]
    fn index_leads_with_fast_tests_no_gated_mutants_and_fast_new_tests() {
        let index = super::help().index().unwrap();
        let lines: Vec<&str> = index.lines().collect();
        let at = |s: &str| lines.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("index lacks {s:?}"));
        let first =
            [at("1. FAST TESTS COME FIRST"), at("2. MUTANTS NEVER IN A GATE"), at("3. A NEW TEST MUST BE FAST TOO")];
        assert!(first.windows(2).all(|w| w[0] < w[1]), "points 1-3 out of order: {first:?}");
        assert!(first[2] < 24, "point 3 is past the first screen (line {})", first[2]);
        assert!(first[2] < at("## The loop") && first[2] < at("## Commands"));
    }

    /// The reader has the binary and its help, nothing else: help names no
    /// design record and no file of slimfit's own repository.
    #[test]
    fn the_manual_names_nothing_the_reader_cannot_open() {
        let manual = super::help().manual().unwrap();
        for unopenable in ["ADR", "docs/", "src/", "testdata/", ".rs`", ".md`"] {
            assert!(!manual.contains(unopenable), "the manual names {unopenable:?}");
        }
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
