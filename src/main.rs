mod config;
mod doctor;
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
/// 3. Run the task's accept command ({const:ACCEPT_COMMANDS}).
/// 4. `slimfit ratchet`: lock the gain into `.slimfit/baseline.json`.
/// 5. Commit the code and the baseline together (one task, one commit); back to 1.
///
/// When setting up slimfit on a repo or a host, run `slimfit doctor` first and do each finding's `fix`. Before adopting slimfit on a repo, read [costs](help:costs) and [setting up in factorium](help:factorium).
///
/// ## Output
///
/// Every command prints one JSON object on stdout with a `next` field: the command to run next. Progress goes to stderr. `slimfit check` exits {const:EXIT_VIOLATION} when a bound is violated; each violation carries its `fix`. Nothing left to do (no task, nothing to ratchet) is an answer and exits {const:EXIT_OK}. A command that cannot finish exits {const:EXIT_ERROR} with `{"error": [{"fault": "...", "fix": "..."}, ...]}`: every fault it found, each with the command or edit that fixes it. Do every fix, then run the command again. Measurements are cached per working tree in `.slimfit/out/`; `--fresh` measures again.
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
    /// Checks how slimfit is set up here and on this host; changes nothing.
    ///
    /// ## When to run it
    ///
    /// First, when setting up slimfit on a repo or a new host; and whenever a command is slower or stranger than it should be. It runs no tests, so it takes about a second.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml`, `.slimfit/baseline.json`, the gates in `factorium.toml` (each `[gate]` entry and the lines of a repository script it names), the host's load, and whether the tools the suites' commands run are installed (on PATH, or in cargo's bin directory). It writes nothing.
    ///
    /// ## What it answers
    ///
    /// - `findings`: in the order to fix them, each with `kind`, `suite` (when it concerns one), `what` (what was found, and where) and `fix` (what to do). Empty when nothing is off.
    /// - `host`: `load` (the 1-minute load average) and `cores`.
    /// - `next`: the first finding's fix, or `slimfit next` when there is none.
    ///
    /// The kinds of finding:
    ///
    /// {table:finding}
    ///
    /// ## What it may refuse
    ///
    /// It exits {const:EXIT_OK} whenever it ran: findings are an answer, not an error. It refuses with exit {const:EXIT_ERROR} only on a bad command line or outside a git repository.
    ///
    /// ## What trips you up
    ///
    /// - It reads gates only from `factorium.toml`, and only one script deep: a gate defined elsewhere (CI, a Makefile), or a test run in a script that the gate's script calls, is not seen.
    /// - A tool is found by name, not run: a tool that is installed but broken, or the wrong version, is not a finding.
    #[command(next_help_heading = "Run first, when setting up on a repo or a host", verbatim_doc_comment)]
    Doctor,
    /// Detects the suites, writes `slimfit.toml` (if missing), measures, and writes the baseline.
    ///
    /// ## When to run it
    ///
    /// Once, when adopting slimfit on a repo, on a quiet host; time it first (see [costs](help:costs)). Run it again only to start the baseline over: it overwrites `.slimfit/baseline.json` with what it measures now, worse bounds included.
    ///
    /// ## What it reads and writes
    ///
    /// It detects {const:LANGUAGES} projects in the repo's files, one suite per project found, each from its language's preset, and writes them to `slimfit.toml` when the repo has none; an existing `slimfit.toml` is kept and used as it is (edit it to override any preset field). It measures every suite once, never from the cache (one instrumented test run, and no mutant), and writes `.slimfit/baseline.json` from that measurement, keeping the loosenings already recorded there. It records the tree as `mutants_at`, unless the baseline already has one: `--mutants` measures the changes made after it (see [mutants scope](help:mutants-scope)). It adds `.slimfit/out` to `.gitignore`, and keeps the measurement and each suite's reports and logs in `.slimfit/out/`.
    ///
    /// ## What it answers
    ///
    /// - `config`: `"written"` or `"kept"`.
    /// - `suites`: each suite's settings, the preset filled in: `name`, `preset`, `root` (its directory in the repo), `sources`, `tests` and `excludes` (globs of source, test and ignored files), `inline_tests` (whether tests sit inside source files), `test_command`, `tests_report` and `coverage_report` (each a `format` and a `path`), `mutants_command`, `mutants_report`, `mutants_diff_flag` (the flag that limits the mutation tool to the lines of a diff; empty when it has none) and `mutants_file_flag` (the flag that limits it to one file; empty when it has none).
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `next`: commit `slimfit.toml`, `.slimfit/baseline.json` and `.gitignore`, then `slimfit next`.
    ///
    /// ## What it may refuse
    ///
    /// - No project of a language it knows, and no `slimfit.toml`: write `slimfit.toml` by hand.
    /// - {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - A test that fails now is recorded as it is: `failures` above 0 in the baseline. Fix it first, then run `init` again.
    /// - On a busy host its `test_seconds` comes out high, and the baseline holds it from then on: run it on a quiet host (see [host load](help:host-load)).
    /// - Run again on a baseline that has `mutants_at`, it keeps it: changes since are still measured by the next `--mutants`, not accepted unmeasured.
    #[command(next_help_heading = "Run once, to adopt slimfit on a repo", verbatim_doc_comment)]
    Init,
    /// The next task to do, with its evidence and accept command.
    ///
    /// ## When to run it
    ///
    /// At the start of each turn of the loop, after the last task's change is ratcheted and committed. Do exactly the task it gives, then run its `accept` command, then `slimfit ratchet` and commit. Tasks come cheapest and safest first, in the order of `--kind`'s values below.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml`, `.slimfit/baseline.json` and the git history of the last {const:DEEPEN_DAYS} days (for `deepen` tasks). It measures the working tree, or reuses the measurement in `.slimfit/out/` when the tree is unchanged since it was taken; it writes nothing else.
    ///
    /// ## What it answers
    ///
    /// - `task`: the first task, or `null` when none is left. With `--all`, `tasks` lists them instead.
    /// - `remaining` (`counts` with `--all`): per kind, how many tasks there are, past the listed ones too.
    /// - `notes`: what was not measured and what that hides: mutants not run, only the change's mutants run, or a mutation tool that does not record which test kills which mutant (only {const:KILLERS_RECORDED} record it, so only they get redundant-test tasks).
    /// - `next`: what to do now.
    ///
    /// A task has `kind`, `suite`, `summary`, `files` (the files to work on), `evidence` (the numbers that make it a task), `steps` (what to do) and `accept` (the command that must pass after). Its `evidence`, by kind:
    ///
    /// - `failing-test`: `test` and `log`, the test run's log.
    /// - `uncovered`: `file`, `start`, `end` and `lines` of the span no test runs, and `function`, when known.
    /// - `redundant-test`: `test`, its `seconds`, `mutants_killed`, and `also_killed_by`, the tests that kill those mutants too.
    /// - `slow-test`: `test`, its `seconds`, and `suite_seconds`, the suite's `test_seconds`.
    /// - `surviving-mutant`: `survivors`, each with its `line` and `mutant` (the change it makes).
    /// - `deepen`: `file`, its `prod_lines`, and `commits_90d`, the commits that changed it in {const:DEEPEN_DAYS} days.
    ///
    /// ## What it may refuse
    ///
    /// {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - Without `--mutants` or `--all-mutants` there are no surviving-mutant or redundant-test tasks: `notes` says so. Those flags cost what [costs](help:costs) says, so never run them in a gate.
    /// - `--mutants` runs only the change's mutants: its tasks name only what they show, and a test it calls redundant is redundant only among them. `--all-mutants` runs every mutant (see [mutants scope](help:mutants-scope)).
    /// - {const:CACHE_PITFALL}
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
    /// Fails if any bound is worse than the baseline; exits {const:EXIT_VIOLATION} on a violation.
    ///
    /// ## When to run it
    ///
    /// After each change, and as a task's `accept` command. In a gate, in place of the project's own test run: `slimfit check` IS that test run, instrumented (see [setting up in factorium](help:factorium)). `--mutants` only on an idle host, never in a gate.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml` and `.slimfit/baseline.json`. It measures the working tree, or reuses the measurement in `.slimfit/out/` when the tree is unchanged since it was taken, and writes nothing but that cache and the suites' reports and logs beside it. The baseline is never written: `slimfit ratchet` does that.
    ///
    /// ## What it holds
    ///
    /// Per suite, against `.slimfit/baseline.json`:
    ///
    /// {table:bound}
    ///
    /// Line counts are recorded and reported, not bounded. To raise a bound, see `slimfit ratchet --help`.
    ///
    /// ## What it answers
    ///
    /// - `verdict`: `ok`, true when no bound is violated; `violations`, {const:VIOLATION}; `improved`, the bounds that got better (`slimfit ratchet` locks them in), each with `suite`, `metric`, `baseline` and `current`; `new_suites`, suites with no baseline yet; and `mutants_unchecked`, true when mutants were not measured.
    /// - `metrics`: per suite, {const:METRICS}.
    /// - `mutated`: per suite, `all` (whether every mutant ran), `files` (the source files the change touches) and `change` (its mutants by outcome: `killed`, `survived`, `unviable`); `null` without mutants (see [mutants scope](help:mutants-scope)).
    /// - `next`: what to do now.
    ///
    /// It exits {const:EXIT_OK} when `ok`, {const:EXIT_VIOLATION} when a bound is violated.
    ///
    /// ## What it may refuse
    ///
    /// {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - It is itself a test run. Put it in the gate in place of the project's own test run, never beside it: running both runs the tests twice.
    /// - `test_seconds` swells on a busy host. slimfit corrects for load, but a violation on a loaded host is worth one `--fresh` run on a quiet one before you chase it (see [host load](help:host-load)).
    /// - A change that only deletes code or tests adds no line, so `--mutants` runs no mutant for it: what a deleted test killed elsewhere is not seen (see [mutants scope](help:mutants-scope)).
    /// - {const:CACHE_PITFALL}
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
    /// ## When to run it
    ///
    /// After a task's `accept` command passes, then commit `.slimfit/baseline.json` with the change it records (one task, one commit). After `slimfit check --mutants` passes, it reuses that measurement and moves `mutants_at` to the tree measured, so the next `--mutants` measures only what changed after it. A survivor that changes nothing observable is accepted with `--loosen survived_mutants --reason "<why>"`.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml` and `.slimfit/baseline.json`, measures (or reuses the measurement in `.slimfit/out/` when the tree is unchanged), and writes `.slimfit/baseline.json`: each bound at the better of baseline and now, a loosened bound at now, with its reason. When the measurement has mutants it also records the tree measured as `mutants_at` (see [mutants scope](help:mutants-scope)).
    ///
    /// ## What it answers
    ///
    /// - `changed`: whether the baseline moved. `false` is nothing to ratchet: an answer (exit {const:EXIT_OK}), and the baseline is not rewritten.
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `loosened`: every loosening kept in the baseline, {const:LOOSENING}.
    /// - `mutants_at`: `commit` and `tree`, what the next `--mutants` measures the change from.
    /// - `next`: commit, then `slimfit next` (or, when nothing changed, just `slimfit next`).
    ///
    /// ## What it may refuse
    ///
    /// It writes nothing when it refuses.
    ///
    /// - A bound got worse: `failures` always; any other unless `--loosen` names it.
    /// - `--loosen` without `--reason`.
    /// - {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - `test_seconds` only moves down, so a baseline set on a quiet host holds; but `--loosen test_seconds` on a busy host loosens it to the busy number. Loosen time only from a `--fresh` run on a quiet host (see [host load](help:host-load)).
    /// - A plain `ratchet` of a tree last measured without mutants keeps `mutants_at`: the change stays to be measured by the next `--mutants`.
    /// - {const:CACHE_PITFALL}
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
    /// ## When to run it
    ///
    /// To see where the codebase stands and how its size moved, e.g. before and after a run of tasks. It decides nothing: `slimfit check` is what holds the bounds.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml`, `.slimfit/baseline.json`, and the source and test files at commits spread over the first-parent history. It measures as `slimfit check` does, without mutants (from the cache in `.slimfit/out/` when the tree is unchanged), and writes nothing else.
    ///
    /// ## What it answers
    ///
    /// - `current`: per suite, the metrics measured now: {const:METRICS}.
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `loosened`: every loosening kept in the baseline, {const:LOOSENING}.
    /// - `history`: oldest first, each point's `commit`, `date` and `suites`: per suite, `prod` and `test` lines.
    /// - `next`: `slimfit next`.
    ///
    /// ## What it may refuse
    ///
    /// {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - `history` counts lines with today's `slimfit.toml`: a commit from before a suite existed counts it as it is configured now.
    /// - {const:CACHE_PITFALL}
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
        .table::<ratchet::Bound>("bound")
        .constant("EXIT_OK", EXIT_OK)
        .constant("EXIT_ERROR", EXIT_ERROR)
        .constant("EXIT_VIOLATION", EXIT_VIOLATION)
        .constant("ACCEPT_COMMANDS", prose_list(tasks::TaskKind::accepts().iter().map(|c| format!("`{c}`")), "or"))
        .constant("LANGUAGES", languages("and"))
        .constant("KILLERS_RECORDED", languages_where(|s| s.mutants_report.format.records_killers()))
        .constant("DIFF_LIMITED", languages_where(|s| !s.mutants_diff_flag.is_empty()))
        .table::<doctor::Kind>("finding")
        .constant("TIME_TOLERANCE", check.time_tolerance)
        .constant("TIME_SLACK", check.time_slack)
        .constant("COVERAGE_SLACK", check.coverage_slack)
        .constant("LISTED_PER_KIND", tasks::LISTED_PER_KIND)
        .constant("LISTED_DEEPEN", tasks::LISTED_DEEPEN)
        .constant("DEEPEN_DAYS", tasks::DEEPEN_DAYS)
        .constant("METRICS", model::METRICS_HELP)
        .constant("VIOLATION", ratchet::VIOLATION_HELP)
        .constant("LOOSENING", ratchet::LOOSENING_HELP)
        .constant("MEASURE_REFUSALS", measure::refusals_help())
        .constant("CACHE_PITFALL", measure::CACHE_HELP)
}

/// What an answer exits with, nothing left to do included.
const EXIT_OK: i32 = 0;
/// What a command that cannot finish exits with.
const EXIT_ERROR: i32 = 2;
/// What `check` exits with when a bound is violated.
const EXIT_VIOLATION: i32 = 1;

/// `items` as prose: `a`, `a and b`, `a, b and c`, with `conjunction` for "and".
fn prose_list(items: impl IntoIterator<Item = String>, conjunction: &str) -> String {
    let mut items: Vec<String> = items.into_iter().collect();
    let last = items.pop().unwrap_or_default();
    if items.is_empty() { last } else { format!("{} {conjunction} {last}", items.join(", ")) }
}

/// The languages slimfit has a preset for.
fn languages(conjunction: &str) -> String {
    prose_list(config::Preset::all().iter().map(|p| p.language().to_string()), conjunction)
}

/// The languages whose preset has `property`.
fn languages_where(property: impl Fn(&config::Suite) -> bool) -> String {
    let presets = config::Preset::all().iter().filter(|p| property(&config::preset(**p)));
    prose_list(presets.map(|p| p.language().to_string()), "and")
}

fn main() {
    let (answer, code) = match run(std::env::args_os()) {
        Ok(done) => done,
        Err(e) => (json!({ "error": fault::of(e) }), EXIT_ERROR),
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
                        format!("no {} project found in {}", languages("or"), repo.display()),
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
                EXIT_OK,
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
                EXIT_OK,
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
            let code = if v.ok { EXIT_OK } else { EXIT_VIOLATION };
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
                EXIT_OK,
            ))
        }
        Cmd::Doctor => {
            let host = doctor::Host::current();
            let findings = doctor::diagnose(&repo, &host);
            let next = findings.first().map_or("slimfit next".to_string(), |f| f.fix.clone());
            Ok((json!({ "findings": findings, "host": host, "next": next }), EXIT_OK))
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
                EXIT_OK,
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
    use crate::{config, model, ratchet, tasks};

    /// The keys `value` is written with, in the order written.
    fn keys(value: &impl serde::Serialize) -> Vec<String> {
        let text = serde_json::to_string(value).unwrap();
        let object = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text).unwrap();
        let mut keys: Vec<String> = object.keys().cloned().collect();
        keys.sort_by_key(|k| text.find(&format!("\"{k}\":")));
        keys
    }

    /// The names `text` writes in backticks, in order.
    fn named(text: &str) -> Vec<String> {
        text.split('`').skip(1).step_by(2).map(String::from).collect()
    }

    /// The line of `page` that starts with `prefix`.
    fn line<'a>(page: &'a str, prefix: &str) -> &'a str {
        page.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no line {prefix:?}"))
    }

    /// Every output key help names is one the code writes, and none is left
    /// out: help describes each key in words, so the list is checked here.
    #[test]
    fn help_names_every_output_key_in_order() {
        assert_eq!(named(model::METRICS_HELP), keys(&model::Metrics::default()));
        let violation = ratchet::Violation::example();
        assert_eq!(named(ratchet::VIOLATION_HELP), keys(&violation));
        let loosening = ratchet::Loosening {
            suite: String::new(),
            metric: ratchet::Bound::Failures,
            from: 0.0,
            to: 0.0,
            reason: String::new(),
        };
        assert_eq!(named(ratchet::LOOSENING_HELP), keys(&loosening));

        let help = super::help();
        let next = help.command_help(&["next"]).unwrap();
        let task = line(&next, "A task has ");
        assert_eq!(named(&task[..task.find("Its `evidence`").unwrap()]), keys(&tasks::Task::example()));
        let init = help.command_help(&["init"]).unwrap();
        let mut suite = named(line(&init, "- `suites`:"));
        suite.remove(0);
        suite.retain(|k| k != "format" && k != "path");
        assert_eq!(suite, keys(&config::preset(config::Preset::Rust)));
        let check = help.command_help(&["check"]).unwrap();
        let improved = line(&check, "- `verdict`:");
        let improved = &improved[improved.find("`improved`").unwrap()..improved.find("; `new_suites`").unwrap()];
        let change =
            ratchet::Change { suite: String::new(), metric: ratchet::Bound::Failures, baseline: 0.0, current: 0.0 };
        assert_eq!(named(improved)[2..], keys(&change)[..]);
    }

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
