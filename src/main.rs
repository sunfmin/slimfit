mod config;
mod doctor;
mod fault;
#[cfg(test)]
mod fixtures;
mod formats;
mod git;
mod landing;
mod loc;
mod measure;
mod model;
mod ratchet;
mod report;
mod select;
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
/// You (the agent) change code; slimfit measures each change and ratchets the result. Never argue with a number: change the code until `slimfit check` passes. Run `slimfit <command> --help` before a command's first run. Your judgment calls are yours; slimfit checks that you made them, not that they were right. What matters most, first:
///
/// 1. FAST TESTS COME FIRST. slimfit must not slow the feedback loop. A gate runs the tests at most once. `slimfit check` IS a test run (instrumented, about 1.5× a plain run): it replaces the project's own test run in that gate, never adds to it.
/// 2. MUTANTS NEVER IN A GATE, ONLY ON A CHANGE, AND ONLY THE FEW YOU PICK. Mutation testing is expensive: never put it in a gate that holds up a landing or a promotion. slimfit never mutates the whole codebase, nor every mutant of a change: you, the agent that made the change, pick the few worth running. `slimfit check --mutants` lists the change's mutants (nothing builds); read your change and pick at most {const:PICKS_MAX}, on the decisions it makes; run them at background priority (in factorium: `factorium worker exclusive --background -- slimfit check --mutants <name>... --why <why>...`); then `slimfit ratchet` records the picks, why, and their outcomes in `.slimfit/mutants/`, and you commit that with the change. When nothing in it is worth a mutant, `slimfit ratchet --no-mutants "<why>"` records that instead. The guarantee is your picks, not every mutant: the gate holds only that a record exists, without running a mutant: its plain `slimfit check` fails while the change adds source lines and no record (see [what the gate holds](help:mutants-scope)). Not as idle work: it commits nothing, so nothing it measures is recorded.
/// 3. A NEW TEST MUST BE FAST TOO. When goals conflict, cover a line with a fast test or delete the code; never with a slow test. This holds for uncovered and surviving-mutant tasks alike.
///
/// ## The loop
///
/// 1. `slimfit next`: one task, with its kind, evidence, files, steps and accept command.
/// 2. Do exactly that task, nothing else.
/// 3. Run the task's accept command ({const:ACCEPT_COMMANDS}). When the change adds source lines, also pick and run its mutants, at background priority (rule 2; `slimfit next` gives that as a `pick-mutants` task).
/// 4. `slimfit ratchet`: lock the gain into `.slimfit/baseline.json` (and, after `--mutants <name>...` or with `--no-mutants "<why>"`, write the change's record in `.slimfit/mutants/`: the gate's `check` fails without it, see [what the gate holds](help:mutants-scope)).
/// 5. Commit the code and `.slimfit/` together (one task, one commit); back to 1.
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
    #[command(subcommand)]
    command: Cmd,
}

/// The repository a command works on; every command but `help` takes it.
#[derive(clap::Args)]
struct At {
    /// The repository to work on (default: the one containing the current directory).
    #[arg(long, verbatim_doc_comment)]
    repo: Option<PathBuf>,
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
    /// It reads `slimfit.toml`, `.slimfit/baseline.json`, the gates and idle work in `factorium.toml` (each `[gate]` entry and `[idle]` step, and the lines of a repository script it names), the host's load, and whether the tools the suites' commands run are installed (on PATH, or in cargo's bin directory). It writes nothing.
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
    Doctor {
        #[command(flatten)]
        at: At,
    },
    /// Detects the suites, writes `slimfit.toml` (if missing), measures, and writes the baseline.
    ///
    /// ## When to run it
    ///
    /// Once, when adopting slimfit on a repo, on a quiet host; time it first (see [costs](help:costs)). Run it again only to start the baseline over: it overwrites `.slimfit/baseline.json` with what it measures now, worse bounds included.
    ///
    /// ## What it reads and writes
    ///
    /// It detects {const:LANGUAGES} projects in the repo's files, one suite per project found, each from its language's preset, and writes them to `slimfit.toml` when the repo has none; an existing `slimfit.toml` is kept and used as it is (edit it to override any preset field). It measures every suite once, never from the cache (one instrumented test run, and no mutant), and writes `.slimfit/baseline.json` from that measurement, keeping the loosenings already recorded there. Code already on the branch changes land on is never charged to a mutants run: only the source lines a change adds after it are (see [what the gate holds](help:mutants-scope)). It adds `.slimfit/out` to `.gitignore`, and keeps the measurement and each suite's reports and logs in `.slimfit/out/`. With `--dry-run` it writes none of these (see [dry runs](help:dry-run)).
    ///
    /// ## What it answers
    ///
    /// - `config`: `"written"` or `"kept"`.
    /// - `suites`: each suite's settings, the preset filled in: `name`, `preset`, `root` (its directory in the repo), `sources`, `tests` and `excludes` (globs of source, test and ignored files), `inline_tests` (whether tests sit inside source files), `build_command` (run first, so the tests' CPU time leaves the build out; empty when the test command builds for itself), `test_command`, `tests_report` and `coverage_report` (each a `format` and a `path`), `reach_report` (where the test run records which functions and lines each test ran; an empty `path` when it does not), `mutants_command`, `mutants_report`, `mutants_diff_flag` (the flag that limits the mutation tool to the lines of a diff; empty when it has none), `mutants_pick_flag` (the flag that runs one mutant by name; empty when it has none) and `mutants_tests` ("reached": a mutant runs only the tests that ran its function, see [mutants scope](help:mutants-scope); "all": every test).
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `next`: commit `slimfit.toml`, `.slimfit/baseline.json` and `.gitignore`, then `slimfit next`.
    /// - `would_write`: with `--dry-run` only, each file it would change, with its whole new text.
    ///
    /// ## What it may refuse
    ///
    /// - No project of a language it knows, and no `slimfit.toml`: write `slimfit.toml` by hand.
    /// - {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - A test that fails now is recorded as it is: `failures` above 0 in the baseline. Fix it first, then run `init` again.
    /// - Run again, it keeps `.slimfit/mutants/`: the records there stay.
    #[command(next_help_heading = "Run once, to adopt slimfit on a repo", verbatim_doc_comment)]
    Init {
        #[command(flatten)]
        at: At,
        /// Measure, and answer what it would write in `would_write`; write nothing (see [dry runs](help:dry-run)).
        #[arg(long, verbatim_doc_comment)]
        dry_run: bool,
        /// Record that nothing in the change is worth a mutant, and why (one line): the record of a change that picks none. No mutant runs.
        #[arg(long, value_name = "WHY", verbatim_doc_comment)]
        no_mutants: Option<String>,
    },
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
    /// - `notes`: what was not measured and what that hides: mutants not run, or a test run that records no coverage per test (redundant-test tasks need it; {const:PER_TEST}'s test run records it).
    /// - `next`: what to do now.
    ///
    /// A task has `kind`, `suite`, `summary`, `files` (the files to work on), `evidence` (the numbers that make it a task), `steps` (what to do) and `accept` (the command that must pass after). Its `evidence`, by kind:
    ///
    /// - `failing-test`: `test` and `log`, the test run's log.
    /// - `pick-mutants`: `lines`, the source lines the change adds, and `candidates`, the change's mutants to pick from: {const:CANDIDATE}.
    /// - `uncovered`: `file`, `start`, `end` and `lines` of the span no test runs, and `function`, when known.
    /// - `redundant-test`: `test`, its `seconds`, `lines` (how many source lines it runs) and `other_test`, the test whose lines it overlaps most: one that runs every one of them too, when there is one.
    /// - `slow-test`: `test`, its `seconds`, and `suite_seconds`, the suite's `test_seconds`.
    /// - `surviving-mutant`: `survivors`, each picked mutant that survived, with its `line` and `mutant` (the change it makes).
    /// - `deepen`: `file`, its `prod_lines`, and `commits_90d`, the commits that changed it in {const:DEEPEN_DAYS} days.
    ///
    /// ## What it may refuse
    ///
    /// {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - It runs no mutant. Surviving-mutant tasks come from the cached `slimfit check --mutants <name>...` of this same tree, the one way mutants run (your picks of your own change, at background priority: [costs](help:costs)); without one there are none, and `notes` says so. `next` has no `--mutants` of its own: it reads that run's survivors, so a second path would only run the same mutants again, at the agent's own priority.
    /// - A pick-mutants task lists the change's mutants as the mutation tool does (`slimfit check --mutants`): that runs the tool's listing, a few seconds, and builds nothing.
    /// - Surviving-mutant tasks name only picks that survived: nothing runs the whole codebase's mutants (see [mutants scope](help:mutants-scope)).
    /// - A redundant-test task is a proposal from coverage, not a verdict: coverage cannot see what a test asserts. Its steps decide it by reading both tests' assertions.
    /// - {const:CACHE_PITFALL}
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Next {
        #[command(flatten)]
        at: At,
        /// List every task, not just the first (at most {const:LISTED_PER_KIND} of each kind per suite, {const:LISTED_DEEPEN} deepen); `counts` counts them all.
        #[arg(long, verbatim_doc_comment)]
        all: bool,
        /// Only tasks of this kind.
        #[arg(long, value_enum, verbatim_doc_comment)]
        kind: Option<tasks::TaskKind>,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Fails if any bound is worse than the baseline; exits {const:EXIT_VIOLATION} on a violation.
    ///
    /// ## When to run it
    ///
    /// After each change, and as a task's `accept` command. In a gate, in place of the project's own test run: `slimfit check` IS that test run, instrumented (see [setting up in factorium](help:factorium)). `--mutants` is run by the agent on its own change: first with no name, to list the mutants to pick from; then with the picks, at background priority (in factorium, `factorium worker exclusive --background -- slimfit check --mutants <name>... --why <why>...`), before `slimfit ratchet`; never in a gate, not as idle work. The plain `check` in the gate holds that the change adds a record of them: `unrecorded_mutants` (see [what the gate holds](help:mutants-scope)).
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml`, `.slimfit/baseline.json` and which files in `.slimfit/mutants/` the change adds. It measures the working tree, or reuses the measurement in `.slimfit/out/` when the tree is unchanged since it was taken, and writes nothing but that cache and the suites' reports and logs beside it. The baseline is never written: `slimfit ratchet` does that.
    ///
    /// ## What it holds
    ///
    /// Per suite, against `.slimfit/baseline.json`, and the change's record in `.slimfit/mutants/`:
    ///
    /// {table:bound}
    ///
    /// Line counts are recorded and reported, not bounded. To raise a bound, see `slimfit ratchet --help`.
    ///
    /// ## What it answers
    ///
    /// - `verdict`: `ok`, true when no bound is violated; `violations`, {const:VIOLATION}; `improved`, the bounds that got better (`slimfit ratchet` locks them in), each with `suite`, `metric`, `baseline` and `current`; `new_suites`, suites with no baseline yet; `mutants_unchecked`, true when mutants were not measured; and `time_unchecked`, the suites whose `test_seconds` was not held: their tests ran inside `--mutants` (see [host load](help:host-load)).
    /// - `landing`: the change the gate holds to a record of its mutants (see [what the gate holds](help:mutants-scope)): {const:LANDING}; `null` when there is no branch it lands on (`slimfit doctor` finds `no_target`).
    /// - `metrics`: per suite, {const:METRICS}.
    /// - `mutated`: per suite, `picks` (the picked mutants it ran, {const:PICK}) and `change` (the picks by outcome: `killed`, `survived`, `unviable`); `null` without mutants (see [mutants scope](help:mutants-scope)).
    /// - `next`: what to do now.
    ///
    /// It exits {const:EXIT_OK} when `ok`, {const:EXIT_VIOLATION} when a bound is violated.
    ///
    /// With `--mutants` and no name it measures nothing and answers `candidates`: per suite, the mutants of the change's lines, as its mutation tool lists them without building, {const:CANDIDATE}; and `next`. It exits {const:EXIT_OK}.
    ///
    /// ## What it may refuse
    ///
    /// {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - It is itself a test run. Put it in the gate in place of the project's own test run, never beside it: running both runs the tests twice.
    /// - A change that only deletes code or tests adds no line: it lists no mutant and needs no record; what a deleted test killed elsewhere is not seen (see [mutants scope](help:mutants-scope)).
    /// - A pick's name holds its line and column: an edit above it renames it, and the old name is refused. List again after editing.
    /// - {const:CACHE_PITFALL}
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Check {
        #[command(flatten)]
        at: At,
        /// Without a name: list the change's mutants to pick from (`candidates`); nothing builds or runs. With names (at most {const:PICKS_MAX}, as listed): also run those picks and check `survived_mutants`, none may survive (see [mutants scope](help:mutants-scope)). On your own change, at background priority; never in a gate.
        #[arg(long, num_args = 0.., value_name = "NAME", verbatim_doc_comment)]
        mutants: Option<Vec<String>>,
        /// Why each pick is worth running, one line per `--mutants` name, in their order; kept in the record `slimfit ratchet` writes.
        #[arg(long, num_args = 1.., value_name = "WHY", verbatim_doc_comment)]
        why: Vec<String>,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Moves the baseline to the current measurement; bounds only tighten.
    ///
    /// ## When to run it
    ///
    /// After a task's `accept` command passes, then commit `.slimfit/baseline.json` with the change it records (one task, one commit). On a change that adds source lines, run your picks of its mutants first, `slimfit check --mutants <name>... --why <why>...` at background priority (in factorium, `factorium worker exclusive --background -- slimfit check --mutants ...`): never in a gate, not as idle work. After it passes, it reuses that measurement and writes the change's record in a file of its own in `.slimfit/mutants/`: commit it with the change, or the gate's `check` fails (see [what the gate holds](help:mutants-scope)). When nothing in the change is worth a mutant, `--no-mutants "<why>"` writes a record of that instead. A survivor that changes nothing observable is accepted with `--loosen survived_mutants --reason "<why>"`.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml` and `.slimfit/baseline.json`, measures (or reuses the measurement in `.slimfit/out/` when the tree is unchanged), and writes `.slimfit/baseline.json`: each bound at the better of baseline and now, a loosened bound at now, with its reason. When the measurement ran picked mutants, or with `--no-mutants`, it also writes `.slimfit/mutants/<hash>.json`, the change's record: the tree it was made of, and per suite the picks ({const:PICK}), or `none`, the reason none was picked; named by its contents (see [what the gate holds](help:mutants-scope)). With `--dry-run` it writes nothing, and caches no measurement (see [dry runs](help:dry-run)).
    ///
    /// ## What it answers
    ///
    /// - `changed`: whether the baseline moved. `false` is nothing to ratchet: an answer (exit {const:EXIT_OK}), and the baseline is not rewritten.
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `loosened`: every loosening kept in the baseline, {const:LOOSENING}.
    /// - `record`: after `--mutants <name>...` or with `--no-mutants`, the change's record, `.slimfit/mutants/<hash>.json`; `null` when there is none.
    /// - `next`: commit, then `slimfit next` (or, when nothing changed, just `slimfit next`).
    /// - `would_write`: with `--dry-run` only, each file it would change (`.slimfit/baseline.json`, the record), with its whole new text.
    ///
    /// ## What it may refuse
    ///
    /// It writes nothing when it refuses.
    ///
    /// - A bound got worse: `failures` always; any other unless `--loosen` names it.
    /// - `--loosen` without `--reason`, and `--loosen unrecorded_mutants`: pick and record the mutants instead.
    /// - `--no-mutants` without a one-line reason, or of a measurement that ran picks (their record is the one).
    /// - {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - It runs no mutant, and takes no `--mutants`: mutants run one way, `slimfit check --mutants` at background priority, and `ratchet` reuses that measurement while the tree is unchanged. A second path would run them at the agent's own priority.
    /// - A `ratchet` of a measurement without picks, and without `--no-mutants`, writes no record: the gate's `check` fails on a change that adds source lines until one is committed.
    /// - {const:CACHE_PITFALL}
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Ratchet {
        #[command(flatten)]
        at: At,
        /// Allow this bound to rise (repeatable); needs `--reason`.
        #[arg(long, value_enum, verbatim_doc_comment)]
        loosen: Vec<ratchet::Bound>,
        /// Why the bound may rise, kept in the baseline: one sentence of why, with the issue it came from. Measurements go in a comment on that issue, not here.
        #[arg(long, verbatim_doc_comment)]
        reason: Option<String>,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
        /// Measure, and answer what it would write in `would_write`; write nothing (see [dry runs](help:dry-run)).
        #[arg(long, verbatim_doc_comment)]
        dry_run: bool,
        /// Record that nothing in the change is worth a mutant, and why (one line): the record of a change that picks none. No mutant runs.
        #[arg(long, value_name = "WHY", verbatim_doc_comment)]
        no_mutants: Option<String>,
    },
    /// Records that a redundant-test candidate is kept: it is not proposed again until its text changes.
    ///
    /// ## When to run it
    ///
    /// When a `redundant-test` task's steps end in KEEP: the assertions showed the test checks what the other cannot take over. Then commit `.slimfit/baseline.json`.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml` and `.slimfit/baseline.json`, and the measurement (from the cache in `.slimfit/out/` when the tree is unchanged, else it measures as `slimfit check` does) for the test's own function: where it is and a hash of its text, from its `fn` line to its closing brace. It writes `.slimfit/baseline.json`'s `kept`: the test, that hash and the reason, replacing an earlier verdict on the same test. `slimfit ratchet` drops a verdict whose test changed or is gone.
    ///
    /// ## What it answers
    ///
    /// - `kept`: every keep verdict in the baseline, {const:KEPT}.
    /// - `next`: commit `.slimfit/baseline.json`, then `slimfit next`.
    ///
    /// ## What it may refuse
    ///
    /// It writes nothing when it refuses.
    ///
    /// - No `--reason`.
    /// - No suite's test run recorded the test (its `test` id is a redundant-test task's `test`).
    /// - Its own function was not found, so there is no text to hold the verdict to.
    /// - {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - The verdict holds against the test's text only: a change to the other test, or to the code both run, does not bring it back. Any edit inside the test does, a reformat included.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Keep {
        #[command(flatten)]
        at: At,
        /// The test kept: a redundant-test task's `test`.
        #[arg(verbatim_doc_comment)]
        test: String,
        /// Why it stays: what it checks that the other test does not, kept in the baseline.
        #[arg(long, verbatim_doc_comment)]
        reason: Option<String>,
    },
    /// Current metrics against the baseline, and line counts over git history.
    ///
    /// ## When to run it
    ///
    /// To see where the codebase stands and how its size moved, e.g. before and after a run of tasks. It decides nothing: `slimfit check` is what holds the bounds.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml`, `.slimfit/baseline.json`, the records in `.slimfit/mutants/`, and the source and test files at commits spread over the first-parent history. It measures as `slimfit check` does, without mutants (from the cache in `.slimfit/out/` when the tree is unchanged), and writes nothing else.
    ///
    /// ## What it answers
    ///
    /// - `current`: per suite, the metrics measured now: {const:METRICS}.
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `loosened`: every loosening kept in the baseline, {const:LOOSENING}.
    /// - `history`: oldest first, each point's `commit`, `date` and `suites`: per suite, `prod` and `test` lines.
    /// - `mutants_records`: how many records `.slimfit/mutants/` holds: {const:RECORDS}. Many without picks, or picks that are all trivial, is a rule kept in form only.
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
        #[command(flatten)]
        at: At,
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

#[doc = include_str!("help/dry-run.md")]
#[derive(Topic)]
#[topic(name = "dry-run")]
struct DryRun;

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
        .topic::<DryRun>()
        .topic::<Factorium>()
        .table::<ratchet::Bound>("bound")
        .constant("EXIT_OK", EXIT_OK)
        .constant("EXIT_ERROR", EXIT_ERROR)
        .constant("EXIT_VIOLATION", EXIT_VIOLATION)
        .constant("ACCEPT_COMMANDS", prose_list(tasks::TaskKind::accepts().iter().map(|c| format!("`{c}`")), "or"))
        .constant("LANGUAGES", languages("and"))
        .constant("DIFF_LIMITED", languages_where(|s| !s.mutants_diff_flag.is_empty()))
        .constant("REACHED", languages_where(|s| s.mutants_tests == config::MutantTests::Reached))
        .constant("PER_TEST", languages_where(|s| !s.reach_report.path.is_empty()))
        .constant("PICKS_MAX", measure::PICKS_MAX)
        .constant("TIMEOUT_FLOOR", select::TIMEOUT_FLOOR)
        .constant("TIMEOUT_FACTOR", select::TIMEOUT_FACTOR)
        .table::<doctor::Kind>("finding")
        .constant("TIME_TOLERANCE", check.time_tolerance)
        .constant("TIME_SLACK", check.time_slack)
        .constant("BUSY_LOAD", doctor::BUSY)
        .constant("COVERAGE_SLACK", check.coverage_slack)
        .constant("LISTED_PER_KIND", tasks::LISTED_PER_KIND)
        .constant("LISTED_DEEPEN", tasks::LISTED_DEEPEN)
        .constant("DEEPEN_DAYS", tasks::DEEPEN_DAYS)
        .constant("COMMON_LINES", format!("{:.0}%", tasks::COMMON_LINES * 100.0))
        .constant("METRICS", model::METRICS_HELP)
        .constant("VIOLATION", ratchet::VIOLATION_HELP)
        .constant("LANDING", landing::HELD_HELP)
        .constant("RECORDS", landing::RECORDS_HELP)
        .constant("CANDIDATE", model::CANDIDATE_HELP)
        .constant("PICK", model::PICK_HELP)
        .constant("LOOSENING", ratchet::LOOSENING_HELP)
        .constant("KEPT", ratchet::KEPT_HELP)
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
    // Run as `cargo-nextest` (first on PATH during a mutants run): the shim
    // that runs only a mutant's tests (ADR 0007).
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.first().and_then(|a| std::path::Path::new(a).file_name()).is_some_and(|n| n == select::SHIM) {
        select::shim(args);
    }
    let (answer, code) = match run(args) {
        Ok(done) => done,
        Err(e) => (json!({ "error": fault::of(e) }), EXIT_ERROR),
    };
    println!("{}", serde_json::to_string_pretty(&answer).expect("json"));
    std::process::exit(code);
}

/// The command line's answer and exit code (help and version print and exit
/// here, as clap does).
fn run(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<(serde_json::Value, i32)> {
    let args: Vec<std::ffi::OsString> = args.into_iter().collect();
    if args.iter().any(|a| a == "--all-mutants") {
        return Err(fault::fault(
            "`--all-mutants` is gone: slimfit never mutates the whole codebase",
            "measure the change's mutants instead: `slimfit check --mutants` (then `slimfit next` for its tasks)",
        ));
    }
    if let Some(cmd) = args.iter().skip(1).find(|a| *a == "next" || *a == "ratchet")
        && args.iter().any(|a| a == "--mutants")
    {
        return Err(fault::fault(
            format!("`{}` takes no `--mutants`: mutants run one way, `slimfit check --mutants`", cmd.display()),
            format!(
                "run `slimfit check --mutants` on your change at background priority; `slimfit {}` then reuses \
                 that measurement of the same tree",
                cmd.display()
            ),
        ));
    }
    // `--help`, `-h` and `help` too: clap prints the manual's answer, on stdout.
    let cli = match command_line(args) {
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
    let at = match &cli.command {
        Cmd::Doctor { at }
        | Cmd::Init { at, .. }
        | Cmd::Next { at, .. }
        | Cmd::Check { at, .. }
        | Cmd::Ratchet { at, .. }
        | Cmd::Keep { at, .. }
        | Cmd::Report { at, .. } => at,
        Cmd::Help { .. } => unreachable!("`parse` answers it"),
    };
    let start = at.repo.clone().unwrap_or(std::env::current_dir()?);
    let repo = git::toplevel(&start)?;
    match cli.command {
        Cmd::Init { dry_run, .. } => {
            let mut writes = Writes { repo: &repo, dry_run, would: Default::default() };
            let created = !repo.join(config::FILE).exists();
            let cfg = if created {
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
                let text = toml::to_string_pretty(&config::Config { suites, check: Default::default() })?;
                writes.put(config::FILE, &text)?;
                config::parse(&repo, &text)
            } else {
                config::load(&repo)
            };
            writes
                .put(".gitignore", &gitignore(&std::fs::read_to_string(repo.join(".gitignore")).unwrap_or_default()))?;
            let (cfg, old) = both(cfg, ratchet::load(&repo))?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh: true, dry_run, timed: false },
            )?;
            let base = ratchet::Baseline {
                suites: m.suites.iter().map(|s| (s.suite.clone(), s.metrics.clone())).collect(),
                loosened: old.loosened,
                kept: old.kept,
                mutants_from: old.mutants_from,
            };
            writes.put(ratchet::FILE, &ratchet::render(&base)?)?;
            Ok((
                writes.answer(json!({
                    "config": if created { "written" } else { "kept" },
                    "suites": cfg.suites.iter().map(|s| s.resolve()).collect::<Vec<_>>(),
                    "baseline": base.suites,
                    "next": "commit slimfit.toml, .slimfit/baseline.json and .gitignore; then `slimfit next`",
                })),
                EXIT_OK,
            ))
        }
        Cmd::Next { all, kind, fresh, .. } => {
            let (cfg, base) = loaded(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh, dry_run: false, timed: false },
            )?;
            let mut list = tasks::all(&repo, &cfg, &m, &base.kept)?;
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
                    "mutants not measured: surviving-mutant tasks need `slimfit check --mutants <name>...` of this \
                     tree first (your picks, at background priority); `slimfit next` then reads its survivors"
                        .to_string(),
                );
            }
            for s in m.suites.iter().filter(|s| s.reach.as_ref().is_none_or(|r| r.lines.is_none())) {
                notes.push(format!(
                    "{}: its test run records no coverage per test, so no redundant test can be proposed",
                    s.suite
                ));
            }
            let next = if list.is_empty() {
                if m.with_mutants {
                    "nothing left to do: the codebase is at its fittest by these measures"
                } else {
                    "nothing left to do but what mutants would show: they run on a change, picked (`slimfit check --mutants`)"
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
        Cmd::Check { mutants: Some(names), why, .. } if names.is_empty() => {
            let (cfg, _) = loaded(&repo)?;
            if !why.is_empty() {
                return Err(fault::fault(
                    "--why without a mutant's name",
                    "give each pick's name to `--mutants`, in the order of `--why`",
                ));
            }
            measure::refuse_unpickable(&cfg)?;
            let candidates = measure::candidates(&repo, &cfg)?;
            let next = if candidates.values().all(Vec::is_empty) {
                "no mutant of the change to pick: when it adds source lines, `slimfit ratchet --no-mutants \"<why>\"`, \
                 then commit .slimfit/"
            } else {
                "read your change and pick at most 5 of `candidates` on the decisions it makes (comparisons, boolean \
                 logic, match guards, a return value that matters), none that cannot build; run them at background \
                 priority: `factorium worker exclusive --background -- slimfit check --mutants <name>... --why \
                 <why>...` (outside factorium, `nice -n 19 slimfit check ...`), then `slimfit ratchet`; or, when none \
                 is worth running, `slimfit ratchet --no-mutants \"<why>\"`"
            };
            Ok((json!({ "candidates": candidates, "next": next }), EXIT_OK))
        }
        Cmd::Check { mutants, why, fresh, .. } => {
            let (cfg, base) = loaded(&repo)?;
            let picks = picks(mutants.unwrap_or_default(), why)?;
            let with_mutants = !picks.is_empty();
            let scope = if with_mutants { measure::Mutants::Picks(picks) } else { measure::Mutants::None };
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: scope, fresh, dry_run: false, timed: !with_mutants },
            )?;
            let mut v = ratchet::check(&base, &m, &cfg.check);
            let (landing, unrecorded) = landing::hold(&repo, &cfg, m.with_mutants)?;
            v.violations.extend(unrecorded);
            v.ok = v.violations.is_empty();
            let next = if !v.ok {
                "fix each violation as its `fix` says, then `slimfit check` again"
            } else if !v.improved.is_empty() || !v.new_suites.is_empty() || with_mutants {
                "slimfit ratchet, then commit"
            } else {
                "slimfit next"
            };
            let code = if v.ok { EXIT_OK } else { EXIT_VIOLATION };
            Ok((
                json!({ "verdict": v, "landing": landing, "metrics": metrics_of(&m), "mutated": mutated_of(&m), "next": next }),
                code,
            ))
        }
        Cmd::Ratchet { loosen, reason, fresh, dry_run, no_mutants, .. } => {
            let (cfg, old) = loaded(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh, dry_run, timed: false },
            )?;
            let new = ratchet::ratchet(&old, &m, &cfg.check, &loosen, reason.as_deref())?;
            let record = record(&m, no_mutants)?.map(|r| r.file()).transpose()?;
            let mut writes = Writes { repo: &repo, dry_run, would: Default::default() };
            let mut changed = false;
            // Nothing to ratchet is an answer, not a failure.
            if new != old {
                writes.put(ratchet::FILE, &ratchet::render(&new)?)?;
                changed = true;
            }
            if let Some((path, text)) = &record
                && !repo.join(path).exists()
            {
                writes.put(path, text)?;
                changed = true;
            }
            let next = if changed {
                "commit .slimfit/ with the change it records; then `slimfit next`"
            } else {
                "nothing to ratchet: the baseline already holds this measurement; `slimfit next`"
            };
            Ok((
                writes.answer(json!({
                    "changed": changed,
                    "baseline": new.suites,
                    "loosened": new.loosened,
                    "record": record.map(|(path, _)| path),
                    "next": next,
                })),
                EXIT_OK,
            ))
        }
        Cmd::Keep { test, reason, .. } => {
            let (cfg, mut base) = loaded(&repo)?;
            let Some(reason) = reason.filter(|r| !r.trim().is_empty()) else {
                return Err(fault::fault(
                    "keep without --reason",
                    "add `--reason \"<what it checks that the other test does not>\"`: it is kept in the baseline",
                ));
            };
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh: false, dry_run: false, timed: false },
            )?;
            let found = m.suites.iter().find_map(|s| {
                let t = s.reach.as_ref()?.tests.iter().find(|t| t.id() == test)?;
                Some((s.suite.clone(), t.source.clone()))
            });
            let Some((suite, source)) = found else {
                return Err(fault::fault(
                    format!("no suite's test run recorded a test {test}"),
                    "name it as the redundant-test task's `test` does: `slimfit next --all --kind redundant-test` lists them",
                ));
            };
            let Some(source) = source else {
                return Err(fault::fault(
                    format!("{test}'s own function was not found, so there is no text to hold the verdict to"),
                    "delete or merge it instead, as the task's steps say",
                ));
            };
            base.kept.retain(|k| !(k.suite == suite && k.test == test));
            base.kept.push(ratchet::Kept { suite, test, hash: source.hash, reason });
            std::fs::write(repo.join(ratchet::FILE), ratchet::render(&base)?)?;
            Ok((json!({ "kept": base.kept, "next": "commit .slimfit/baseline.json; then `slimfit next`" }), EXIT_OK))
        }
        Cmd::Doctor { .. } => {
            let host = doctor::Host::current();
            let findings = doctor::diagnose(&repo, &host);
            let next = findings.first().map_or("slimfit next".to_string(), |f| f.fix.clone());
            Ok((json!({ "findings": findings, "host": host, "next": next }), EXIT_OK))
        }
        Cmd::Help { .. } => unreachable!("`parse` answers it"),
        Cmd::Report { commits, points, fresh, .. } => {
            let (cfg, base) = loaded(&repo)?;
            let m = measure::current(
                &repo,
                &cfg,
                &measure::Options { mutants: measure::Mutants::None, fresh, dry_run: false, timed: false },
            )?;
            let history = report::history(&repo, &cfg, commits, points)?;
            let records = landing::records(&repo)?;
            Ok((
                json!({
                    "current": metrics_of(&m),
                    "baseline": base.suites,
                    "loosened": base.loosened,
                    "history": history,
                    "mutants_records": records,
                    "next": "slimfit next",
                }),
                EXIT_OK,
            ))
        }
    }
}

/// slimfit.toml and the baseline, or the faults of both.
fn loaded(repo: &std::path::Path) -> Result<(config::Config, ratchet::Baseline)> {
    both(config::load(repo), ratchet::load(repo))
}

/// Both, or the faults of both.
fn both(c: Result<config::Config>, b: Result<ratchet::Baseline>) -> Result<(config::Config, ratchet::Baseline)> {
    match (c, b) {
        (Ok(c), Ok(b)) => Ok((c, b)),
        (c, b) => Err(fault::Faults(c.err().into_iter().chain(b.err()).flat_map(fault::of).collect()).into()),
    }
}

/// The command line, read against the manual. What `--help`, `-h` and
/// `help [<topic>]` answer comes back as the error clap prints on stdout,
/// exiting 0; a topic that does not exist, as one it prints on stderr,
/// exiting 2, as for any other bad argument.
/// The command line, parsed without the manual when it needs none: rendering
/// the manual's pages took most of a short run (121M of 135M instructions for
/// `next`, slimfit#27). Help, and a line clap refuses, go through the manual.
fn command_line(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Cli, clap::Error> {
    let args: Vec<_> = args.into_iter().collect();
    match Cli::try_parse_from(&args) {
        Ok(cli) if !matches!(cli.command, Cmd::Help { .. }) => Ok(cli),
        _ => parse(&help(), args),
    }
}

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

/// The picks `check --mutants <name>... --why <why>...` names: at most
/// `PICKS_MAX`, each with its own one-line reason.
fn picks(names: Vec<String>, why: Vec<String>) -> Result<Vec<model::Pick>> {
    let mut faults = Vec::new();
    if names.len() > measure::PICKS_MAX {
        faults.push(fault::Fault {
            fault: format!("{} mutants picked, more than {}", names.len(), measure::PICKS_MAX),
            fix: format!(
                "pick at most {}: those on the decisions the change makes (comparisons, boolean logic, match \
                 guards, a return value that matters)",
                measure::PICKS_MAX
            ),
        });
    }
    if names.len() != why.len() || why.iter().any(|w| w.trim().is_empty() || w.contains('\n')) {
        faults.push(fault::Fault {
            fault: format!("{} mutant(s) picked, with {} `--why`", names.len(), why.len()),
            fix: "give each pick one `--why` of one line, in the order of the names".into(),
        });
    }
    if !faults.is_empty() {
        return Err(fault::Faults(faults).into());
    }
    Ok(names.into_iter().zip(why).map(|(name, why)| model::Pick { name, why, outcome: None }).collect())
}

/// The record `ratchet` writes of `m`: its picks, or, with `--no-mutants`,
/// why none was picked; `None` when there is neither.
fn record(m: &model::Measurement, no_mutants: Option<String>) -> Result<Option<landing::Record>> {
    let picks: std::collections::BTreeMap<String, Vec<model::Pick>> = m
        .suites
        .iter()
        .filter_map(|s| Some((s.suite.clone(), s.mutated.as_ref()?.picks.clone())))
        .filter(|(_, p)| !p.is_empty())
        .collect();
    let none = match no_mutants {
        Some(_) if !picks.is_empty() => {
            return Err(fault::fault(
                "--no-mutants, but this tree's measurement ran picked mutants",
                "leave out `--no-mutants`: the record is of those picks (or `--fresh` to measure without them)",
            ));
        }
        Some(why) if why.trim().is_empty() || why.contains('\n') => {
            return Err(fault::fault(
                "--no-mutants without a reason",
                "say in one line why nothing in the change is worth a mutant",
            ));
        }
        none => none,
    };
    if picks.is_empty() && none.is_none() {
        return Ok(None);
    }
    Ok(Some(landing::Record { tree: m.tree.clone(), picks, none }))
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

/// The files a command writes, by path in the repo: written at once, or in
/// a dry run only collected, those that would change, for its answer.
struct Writes<'a> {
    repo: &'a std::path::Path,
    dry_run: bool,
    would: serde_json::Map<String, serde_json::Value>,
}

impl Writes<'_> {
    fn put(&mut self, path: &str, text: &str) -> Result<()> {
        let file = self.repo.join(path);
        if std::fs::read_to_string(&file).is_ok_and(|old| old == text) {
            return Ok(());
        }
        if self.dry_run {
            self.would.insert(path.into(), text.into());
        } else {
            std::fs::create_dir_all(file.parent().expect("a path in the repo"))?;
            std::fs::write(file, text)?;
        }
        Ok(())
    }

    /// The command's answer; in a dry run with `would_write`, and `next`
    /// says to run it for real.
    fn answer(self, mut v: serde_json::Value) -> serde_json::Value {
        if self.dry_run {
            v["would_write"] = self.would.into();
            v["next"] = "if `would_write` is what you want, run the same command without --dry-run".into();
        }
        v
    }
}

/// `.gitignore`'s text with the output directory ignored: added once, on a
/// line of its own; an existing `/.../` entry counts.
fn gitignore(text: &str) -> String {
    if text.lines().any(|l| l.trim() == measure::OUT || l.trim() == format!("/{}/", measure::OUT)) {
        return text.into();
    }
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    format!("{text}{sep}{}\n", measure::OUT)
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
        let held = crate::landing::Held { target: String::new(), from: String::new(), lines: 0, records: vec![] };
        assert_eq!(named(crate::landing::HELD_HELP), keys(&held));
        assert_eq!(named(crate::landing::RECORDS_HELP), keys(&crate::landing::Records::default()));
        let candidate = model::Candidate {
            name: String::new(),
            file: String::new(),
            line: 0,
            function: String::new(),
            replacement: String::new(),
        };
        assert_eq!(named(model::CANDIDATE_HELP), keys(&candidate));
        let pick = model::Pick { name: String::new(), why: String::new(), outcome: None };
        assert_eq!(named(model::PICK_HELP)[..3], keys(&pick)[..]);
        let check = super::help().command_help(&["check"]).unwrap();
        assert!(check.contains(model::CANDIDATE_HELP) && check.contains(model::PICK_HELP), "{check}");
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
        for args in ["slimfit --help", "slimfit -h", "slimfit help"] {
            assert_eq!(answer(args), (ErrorKind::DisplayHelp, index.clone()), "{args}");
        }
        assert_eq!(answer("slimfit check --help"), (ErrorKind::DisplayHelp, help.command_help(&["check"]).unwrap()));
        assert_eq!(answer("slimfit help costs"), (ErrorKind::DisplayHelp, help.topic_help("costs").unwrap()));
        assert_eq!(answer("slimfit help all"), (ErrorKind::DisplayHelp, help.manual().unwrap()));
        let (kind, text) = answer("slimfit help nope");
        assert_eq!(kind, ErrorKind::InvalidValue);
        assert!(text.contains("costs"), "{text}");
        // A repository means nothing to the manual: `help` takes none.
        assert_eq!(answer("slimfit help --repo x").0, ErrorKind::UnknownArgument);
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
    use super::{gitignore, measure::OUT};

    /// The output directory is ignored once: added after any text (on its own
    /// line), never twice, and an existing `/.../` entry counts.
    #[test]
    fn gitignore_adds_the_output_dir_once() {
        assert_eq!(gitignore(""), format!("{OUT}\n"));
        assert_eq!(gitignore("target"), format!("target\n{OUT}\n"));
        assert_eq!(gitignore("target\n"), format!("target\n{OUT}\n"));
        for kept in [format!("a\n {OUT} \nb"), format!("/{OUT}/\nb\n"), format!("b\n/{OUT}/")] {
            assert_eq!(gitignore(&kept), kept);
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
        let mut all: Vec<std::ffi::OsString> = vec!["slimfit".into()];
        all.extend(args.split_whitespace().map(Into::into));
        all.extend(["--repo".into(), repo.into()]);
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

    /// `slimfit` with `args` as given, spaces in them kept.
    fn slimfit_args(repo: &Path, args: &[&str]) -> anyhow::Result<(Value, i32)> {
        let mut all: Vec<std::ffi::OsString> = vec!["slimfit".into()];
        all.extend(args.iter().map(Into::into));
        all.extend(["--repo".into(), repo.into()]);
        run(all)
    }

    /// The gate's plain `check` holds a landing that adds source lines to a
    /// record of its mutants (ADR 0008): it fails while the change adds
    /// none, and passes once it adds one, of picks or of a reason none is
    /// worth one, whatever lines it covers. `next` asks for the picks first.
    /// Two branches that each recorded their own change merge without a
    /// conflict, and `report` counts their records.
    #[test]
    fn a_landing_passes_only_with_a_record_of_its_mutants() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        cfg.suites[0].overrides.mutants_tests = Some(crate::config::MutantTests::All);
        cfg.check.target = Some("target".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        std::fs::write(repo.join(".gitignore"), ".slimfit/out\n").unwrap();
        slimfit(repo, "ratchet").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "adopt"]);
        git(repo, &["branch", "target"]);
        let held = |want: i32| {
            let (out, code) = slimfit(repo, "check").unwrap();
            assert_eq!(code, want, "{out:#}");
            out
        };
        let add = |file: &str, text: &str| {
            let p = repo.join(file);
            let old = std::fs::read_to_string(&p).unwrap_or_default();
            std::fs::write(p, old + text).unwrap();
        };
        let commit = |branch: &str| {
            git(repo, &["add", "-A"]);
            git(repo, &["commit", "-qm", branch]);
        };
        let killed = "src/lib.rs:12:9: replace >= with < in is_adult";

        git(repo, &["checkout", "-qb", "a"]);
        // Nothing added: nothing to pick, and no record due.
        let (listed, _) = slimfit(repo, "check --mutants").unwrap();
        assert_eq!(listed["candidates"]["rust"], serde_json::json!([]), "{listed:#}");
        assert!(listed["next"].as_str().unwrap().starts_with("no mutant of the change to pick"), "{listed:#}");
        add("src/lib.rs", "\npub fn a_one() -> u8 {\n    1\n}\n");
        let out = held(crate::EXIT_VIOLATION);
        let v = &out["verdict"]["violations"][0];
        assert_eq!((v["metric"].as_str(), v["current"].as_f64()), (Some("unrecorded_mutants"), Some(3.0)), "{out:#}");
        assert!(v["fix"].as_str().unwrap().contains("slimfit check --mutants <name>... --why <why>..."), "{v:#}");
        assert_eq!((out["landing"]["lines"].as_u64(), &out["landing"]["records"]), (Some(3), &serde_json::json!([])));
        // `next` asks for the picks first, with the candidates to pick from.
        let (next, _) = slimfit(repo, "next").unwrap();
        assert_eq!(next["task"]["kind"], "pick-mutants", "{next:#}");
        assert_eq!(next["task"]["evidence"]["candidates"].as_array().unwrap().len(), 17, "{next:#}");
        let (listed, code) = slimfit(repo, "check --mutants").unwrap();
        assert_eq!((listed["candidates"]["rust"][16]["name"].as_str(), code), (Some(killed), 0));
        // Committing it changes nothing: the change is the branch's, not the working tree's.
        git(repo, &["commit", "-qam", "a, unrecorded"]);
        held(crate::EXIT_VIOLATION);
        // Too many picks, or a pick without its reason: refused.
        let six = format!("check --mutants {} --why {}", ["n"; 6].join(" "), ["w"; 6].join(" "));
        assert!(faults(repo, &six)[0].fault.contains("6 mutants picked, more than 5"));
        assert!(faults(repo, "check --mutants n m --why w")[0].fault.contains("2 mutant(s) picked, with 1 `--why`"));
        assert_eq!(faults(repo, "check --mutants --why w")[0].fault, "--why without a mutant's name");
        let (out, code) = slimfit_args(repo, &["check", "--mutants", killed, "--why", "the age bound"]).unwrap();
        assert_eq!((code, out["mutated"]["rust"]["picks"][0]["outcome"].as_str()), (0, Some("killed")), "{out:#}");
        let no = crate::fault::of(slimfit_args(repo, &["ratchet", "--no-mutants", "nothing"]).unwrap_err());
        assert!(no[0].fault.contains("--no-mutants, but this tree's measurement ran picked mutants"), "{no:?}");
        let (out, _) = slimfit(repo, "ratchet").unwrap();
        let record = out["record"].as_str().unwrap().to_string();
        assert!(record.starts_with(".slimfit/mutants/"), "{out:#}");
        let text = std::fs::read_to_string(repo.join(&record)).unwrap();
        assert!(text.contains("\"why\": \"the age bound\"") && text.contains("\"outcome\": \"killed\""), "{text}");
        commit("a");
        assert_eq!(held(crate::EXIT_OK)["landing"]["records"], serde_json::json!([record]));
        // Which lines a record covers is not held: an edit after it passes.
        let text = std::fs::read_to_string(repo.join("src/lib.rs")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), text.replace("    1\n}\n", "    2\n}\n")).unwrap();
        held(crate::EXIT_OK);
        git(repo, &["checkout", "--", "src/lib.rs"]);

        // Another branch records that nothing in its change is worth a mutant.
        git(repo, &["checkout", "-qb", "b", "target"]);
        add("src/b.rs", "pub fn b_two() -> u8 {\n    2\n}\n");
        held(crate::EXIT_VIOLATION);
        assert!(faults(repo, "ratchet --no-mutants")[0].fault.contains("a value is required"));
        let blank = crate::fault::of(slimfit_args(repo, &["ratchet", "--no-mutants", " "]).unwrap_err());
        assert_eq!(blank[0].fault, "--no-mutants without a reason");
        let (out, _) = slimfit_args(repo, &["ratchet", "--no-mutants", "a constant: no decision to mutate"]).unwrap();
        let none = std::fs::read_to_string(repo.join(out["record"].as_str().unwrap())).unwrap();
        assert!(none.contains("\"none\": \"a constant: no decision to mutate\"") && !none.contains("picks"), "{none}");
        commit("b");
        held(crate::EXIT_OK);
        git(repo, &["checkout", "-q", "a"]);
        git(repo, &["merge", "-q", "--no-edit", "b"]);
        let out = held(crate::EXIT_OK);
        assert_eq!(
            (out["landing"]["lines"].as_u64(), out["landing"]["records"].as_array().unwrap().len()),
            (Some(6), 2)
        );
        // A per-line record from before picks counts as older.
        std::fs::write(repo.join(crate::landing::DIR).join("old.json"), r#"{"lines": {}}"#).unwrap();
        let (report, _) = slimfit(repo, "report --points 1").unwrap();
        let counts = serde_json::json!({ "with_picks": 1, "without_picks": 1, "older": 1 });
        assert_eq!(report["mutants_records"], counts, "{report:#}");
        // A record that does not parse, and a target that is not there, are refused.
        std::fs::write(repo.join(crate::landing::DIR).join("x.json"), "{").unwrap();
        let f = faults(repo, "report");
        assert!(f[0].fault.contains("x.json is not a record of picked mutants"), "{f:?}");
        assert_eq!(f[0].fix, "restore it from git: `git checkout -- .slimfit/mutants`");
        std::fs::remove_file(repo.join(crate::landing::DIR).join("x.json")).unwrap();
        std::fs::remove_file(repo.join(crate::landing::DIR).join("old.json")).unwrap();
        cfg.check.target = Some("gone".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        let f = faults(repo, "check");
        assert!(f[0].fault.starts_with("slimfit.toml's [check] target gone shares no commit with HEAD"), "{f:?}");
    }

    /// Code from before the rule is not charged (slimfit#37): the baseline's
    /// `mutants_from` moves where a landing starts while the target is
    /// behind it, and a line added after it is held as ever. A cut that is
    /// not on HEAD's history counts for nothing.
    #[test]
    fn code_from_before_the_rule_is_not_charged() {
        let (dir, mut cfg) = sample_repo();
        let repo = dir.path();
        cfg.check.target = Some("target".into());
        std::fs::write(repo.join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        slimfit(repo, "ratchet").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "adopt"]);
        git(repo, &["branch", "target"]);
        let add = |file: &str, text: &str| {
            let p = repo.join(file);
            std::fs::write(&p, std::fs::read_to_string(&p).unwrap() + text).unwrap();
        };
        let cut = |at: &str| {
            let p = repo.join(crate::ratchet::FILE);
            let mut b: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            b["mutants_from"] = at.into();
            std::fs::write(p, b.to_string()).unwrap();
        };
        let out = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(["-c", "user.email=t@slimfit", "-c", "user.name=t", "-C"])
                .arg(repo)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8(o.stdout).unwrap().trim().to_string()
        };
        let held = |want: i32| {
            let (out, code) = slimfit(repo, "check").unwrap();
            assert_eq!(code, want, "{out:#}");
            (out["landing"]["from"].as_str().unwrap().to_string(), out["landing"]["lines"].as_u64().unwrap())
        };
        git(repo, &["checkout", "-qb", "a"]);
        add("src/lib.rs", "\npub fn before_the_rule() -> u8 {\n    1\n}\n");
        git(repo, &["commit", "-qam", "before the rule"]);
        let before = out(&["rev-parse", "HEAD"]);
        cut(&before);
        let (from, n) = held(crate::EXIT_OK);
        assert_eq!((from.as_str(), n), (before.as_str(), 0));
        // A ratchet keeps the cut.
        slimfit(repo, "ratchet").unwrap();
        assert_eq!(crate::ratchet::load(repo).unwrap().mutants_from.as_deref(), Some(before.as_str()));
        // A line added after the cut is the change's, with no record: check fails.
        add("src/lib.rs", "\npub fn after_the_rule() -> u8 {\n    2\n}\n");
        assert_eq!(held(crate::EXIT_VIOLATION), (before.clone(), 3));
        // A cut not on HEAD's history (or no commit at all) moves nothing.
        let target = out(&["rev-parse", "target"]);
        for elsewhere in [out(&["commit-tree", "-m", "x", "-p", "target", "target^{tree}"]), "0".repeat(40)] {
            cut(&elsewhere);
            assert_eq!(held(crate::EXIT_VIOLATION), (target.clone(), 6));
        }
    }

    /// No task left of a kind, and nothing to ratchet, are answers: exit 0.
    #[test]
    fn nothing_to_do_is_an_answer() {
        let (dir, cfg) = sample_repo();
        std::fs::write(dir.path().join("slimfit.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        let (first, code) = slimfit(dir.path(), "ratchet").unwrap();
        assert_eq!((first["changed"].as_bool(), code), (Some(true), 0));
        // No mutants ran, and none is said to be worth one: no record.
        assert_eq!(first["record"], Value::Null, "{first}");
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
