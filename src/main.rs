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
/// 3. Run the task's accept command (`slimfit check` or `slimfit check --mutants`).
/// 4. `slimfit ratchet`: lock the gain into `.slimfit/baseline.json`.
/// 5. Commit the code and the baseline together (one task, one commit); back to 1.
///
/// Before adopting slimfit on a repo, read [costs](help:costs) and [setting up in factorium](help:factorium).
///
/// ## Output
///
/// Every command prints one JSON object on stdout with a `next` field: the command to run next. Progress goes to stderr. `slimfit check` exits 1 when a bound is violated; each violation carries its `fix`. Nothing left to do (no task, nothing to ratchet) is an answer and exits 0. A command that cannot finish exits 2 with `{"error": [{"fault": "...", "fix": "..."}, ...]}`: every fault it found, each with the command or edit that fixes it. Do every fix, then run the command again. Measurements are cached per working tree in `.slimfit/out/`; `--fresh` measures again.
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
    /// It writes `slimfit.toml` when the repo has none, and keeps it otherwise. It measures every suite once (one instrumented test run, never from the cache) and writes `.slimfit/baseline.json` from that measurement, keeping any loosened bounds already there. It runs no mutant: it records the tree as `mutants_at`, and `--mutants` measures the changes made after it (see [mutants scope](help:mutants-scope)). It adds `.slimfit/out` to `.gitignore`.
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
    Init,
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
    /// Without `--mutants` or `--all-mutants` there are no surviving-mutant or redundant-test tasks: `notes` says so. `--mutants` runs only the change's mutants, so its tasks name only what they show; `--all-mutants` runs every mutant. Those flags cost what [costs](help:costs) says.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Next {
        /// List every task, not just the first (at most {const:LISTED_PER_KIND} of each kind per suite, {const:LISTED_DEEPEN} deepen); `counts` counts them all.
        #[arg(long, verbatim_doc_comment)]
        all: bool,
        /// Only tasks of this kind.
        #[arg(long, value_enum, verbatim_doc_comment)]
        kind: Option<tasks::TaskKind>,
        /// Measure the mutants of the change too, the lines changed since the baseline's `mutants_at`: tasks from what they show (see [mutants scope](help:mutants-scope)).
        #[arg(long, verbatim_doc_comment)]
        mutants: bool,
        /// Measure every mutant: tasks from the whole suite's mutants, redundant tests across it.
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
    /// - `survived_mutants`: 0 in the lines the change added, checked only with `--mutants`: every mutant of new or edited code is killed. Survivors elsewhere are tasks, not violations; no baseline keeps a count (see [mutants scope](help:mutants-scope)).
    ///
    /// Line counts are recorded and reported, not bounded. To raise a bound, see `slimfit ratchet --help`.
    ///
    /// ## What it answers
    ///
    /// - `verdict`: `ok`; `violations`, each with `suite`, `metric`, `baseline`, `limit`, `current` and `fix` (what to do); `improved`, the bounds that got better (`slimfit ratchet` locks them in); `new_suites`, suites with no baseline yet; and `mutants_unchecked`, true when mutants were not measured.
    /// - `metrics`: per suite, `prod_lines`, `test_lines`, `tests`, `failures`, `test_seconds`, `covered_lines` and `uncovered_lines`.
    /// - `mutated`: per suite, `all` (whether every mutant ran), `files` (the source files the change touches) and `change` (its mutants by outcome: `killed`, `survived`, `unviable`); `null` without mutants (see [mutants scope](help:mutants-scope)).
    /// - `next`: what to do now.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Check {
        /// Also check `survived_mutants`: run the mutants of the lines changed since the baseline's `mutants_at`; none may survive (see [mutants scope](help:mutants-scope)). Never in a gate.
        #[arg(long, verbatim_doc_comment)]
        mutants: bool,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Moves the baseline to the current measurement; bounds only tighten.
    ///
    /// Run it after a task's accept command passes, then commit `.slimfit/baseline.json` with the change it records. It refuses, writing nothing, when a bound got worse, unless `--loosen` allows that bound to rise; `failures` never may. A loosening's reason is kept in the baseline, and every report shows it.
    ///
    /// When the measurement has mutants (`--mutants`, or a cached `slimfit check --mutants` of the same tree), it moves `mutants_at` to the tree measured: its change is accepted, and the next `--mutants` measures only what changed after it. A survivor that changes nothing observable is accepted with `--loosen survived_mutants --reason "<why>"`.
    ///
    /// ## What it answers
    ///
    /// - `changed`: whether the baseline moved. `false` is nothing to ratchet: an answer (exit 0), and the baseline is not rewritten.
    /// - `baseline`: per suite, the metrics now held.
    /// - `loosened`: every loosening kept in the baseline, with its reason.
    /// - `mutants_at`: the tree the next `--mutants` measures from.
    /// - `next`: commit, then `slimfit next` (or, when nothing changed, just `slimfit next`).
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Ratchet {
        /// Allow this bound to rise (repeatable); needs `--reason`.
        #[arg(long, value_enum, verbatim_doc_comment)]
        loosen: Vec<ratchet::Bound>,
        /// Why the bound may rise, kept in the baseline.
        #[arg(long, verbatim_doc_comment)]
        reason: Option<String>,
        /// Measure the change's mutants first, as `slimfit check --mutants` does.
        #[arg(long, verbatim_doc_comment)]
        mutants: bool,
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
        .topic::<Factorium>()
        .constant("TIME_TOLERANCE", check.time_tolerance)
        .constant("TIME_SLACK", check.time_slack)
        .constant("COVERAGE_SLACK", check.coverage_slack)
        .constant("LISTED_PER_KIND", tasks::LISTED_PER_KIND)
        .constant("LISTED_DEEPEN", tasks::LISTED_DEEPEN)
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
    // `--help`, `-h` and `help` too: clap prints the manual's answer, on stdout.
    let cli = match parse(&help(), args) {
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
        Cmd::Init => {
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
            Ok((
                if all {
                    json!({ "tasks": list, "counts": counts, "notes": notes, "next": next })
                } else {
                    json!({ "task": list.first(), "remaining": counts, "notes": notes, "next": next })
                },
                0,
            ))
        }
        Cmd::Check { mutants, fresh } => {
            let (cfg, base) = loaded(&repo)?;
            let m = measure::current(&repo, &cfg, &base, &measure::Options { mutants: scope(mutants, false), fresh })?;
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
        Cmd::Ratchet { loosen, reason, mutants, fresh } => {
            let (cfg, old) = loaded(&repo)?;
            let m = measure::current(&repo, &cfg, &old, &measure::Options { mutants: scope(mutants, false), fresh })?;
            let new = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            // Nothing to ratchet is an answer, not a failure.
            let changed = new != old;
            if changed {
                ratchet::save(&repo, &new)?;
            }
            let next = if changed {
                "commit .slimfit/baseline.json with the change it records; then `slimfit next`"
            } else {
                "nothing to ratchet: the baseline already holds this measurement; `slimfit next`"
            };
            Ok((
                json!({
                    "changed": changed,
                    "baseline": new.suites,
                    "loosened": new.loosened,
                    "mutants_at": new.mutants_at,
                    "next": next,
                }),
                0,
            ))
        }
        Cmd::Help { .. } => unreachable!("`parse` answers it"),
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
