//! The command line, read against the manual: its commands and flags, the
//! manual's pages and topics, and what slimfit exits with.

use crate::{config, doctor, fault, growth, landing, measure, model, ratchet, report, select, tasks};
use agent_help::{Help, Topic};
use anyhow::Result;
use clap::{FromArgMatches, Parser, Subcommand};
use std::path::PathBuf;

/// Keeps a codebase small and its tests few, fast and strong.
///
/// You (the agent) change code; slimfit measures each change and ratchets the result. Never argue with a number: change the code until `slimfit check` passes. Run `slimfit <command> --help` before a command's first run. Your judgment calls are yours; slimfit checks that you made them, not that they were right. What matters most, first:
///
/// 1. FAST TESTS COME FIRST. slimfit must not slow the feedback loop. A gate runs the tests at most once. `slimfit check` IS a test run (instrumented, about 1.5× a plain run): it replaces the project's own test run in that gate, never adds to it.
/// 2. MUTANTS NEVER IN A GATE, ONLY ON A CHANGE, AND ONLY THE FEW YOU PICK. Mutation testing is expensive: never put it in a gate that holds up a landing or a promotion. slimfit never mutates the whole codebase, nor every mutant of a change: a second agent, never you who made the change, picks the few worth running, given only [the picker's goal](help:picker), your diff and the candidates. `slimfit check --mutants` lists the change's mutants (nothing builds); the picker picks at most {const:PICKS_MAX}; you run them at background priority (in factorium: `factorium worker exclusive --background -- slimfit check --mutants <name>... --why <why>...`); then `slimfit ratchet --picker <its id> --author <yours>` records the picks, why, who picked and their outcomes in `.slimfit/mutants/`, and you commit that with the change. When the picker finds nothing worth a mutant, `slimfit ratchet --no-mutants "<its why>" --picker <its id> --author <yours>` records that instead. The guarantee is the picks, not every mutant: the gate holds only that a record exists and names a picker other than its author, without running a mutant: its plain `slimfit check` fails while the change adds source lines and no such record (see [what the gate holds](help:mutants-scope)). Not as idle work: it commits nothing, so nothing it measures is recorded.
/// 3. A NEW TEST MUST BE FAST TOO. When goals conflict, cover a line with a fast test or delete the code; never with a slow test. This holds for uncovered and surviving-mutant tasks alike. A test that covers a line asserts what the line decides: one that only runs it is no fix, and no bound asks for it (`check` fails on no coverage number).
/// 4. CODE GROWS ONLY FOR A REASON A SECOND AGENT ACCEPTED. A change that adds more than `growth_allowance` source lines net (by default {const:GROWTH_ALLOWANCE}; code it deletes counts against them) carries a growth record in `.slimfit/growth/`: the issue it serves, the existing code you weighed reusing or deleting, and the verdict of a second agent, never you, given only [the reviewer's goal](help:reviewer) and your diff: `as-is` with its why, or what to cut, which you then do or answer. `slimfit ratchet --serves ... --considered ... --as-is "<its why>" --reviewer <its id> --author <yours>` writes it. The gate's plain `slimfit check` fails without one (`unrecorded_growth`), never on the number alone.
///
/// ## The loop
///
/// 1. `slimfit next`: one task, with its kind, evidence, files, steps and accept command.
/// 2. Do exactly that task, nothing else.
/// 3. Run the task's accept command ({const:ACCEPT_COMMANDS}). When the change adds source lines, also have a second agent pick its mutants, and run them at background priority (rule 2; `slimfit next` gives that as a `pick-mutants` task); when it adds more than the growth allowance net, have a second agent judge whether it could be smaller (rule 4; a `justify-growth` task).
/// 4. `slimfit ratchet`: lock the gain into `.slimfit/baseline.json` (and, after `--mutants <name>...` or with `--no-mutants "<why>"`, with `--picker` and `--author`, write the change's record in `.slimfit/mutants/`: the gate's `check` fails without it, see [what the gate holds](help:mutants-scope); and with `--serves`, `--considered`, a verdict and `--reviewer`, the growth record in `.slimfit/growth/`, see [the reviewer](help:reviewer)).
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
pub(crate) struct At {
    /// The repository to work on (default: the one containing the current directory).
    #[arg(long, verbatim_doc_comment)]
    repo: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
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
    /// It detects {const:LANGUAGES} projects in the repo's files, one suite per project found, each from its language's preset, and writes them to `slimfit.toml` when the repo has none; an existing `slimfit.toml` is kept and used as it is (edit it to override any preset field). A Rust suite's `tests` globs are, by default, {const:RUST_TESTS}: tests, benches and examples in any crate. It counts as test code, besides its `tests` globs and its `#[cfg(test)]` code, every file of a workspace crate that other members of the workspace at the suite's root reach only through `[dev-dependencies]` (none through `[dependencies]` or `[build-dependencies]`): read from the Cargo manifests, never declared again in `slimfit.toml`; for a layout this misses, add globs to the suite's `tests`. It measures every suite once, never from the cache (one instrumented test run, and no mutant), and writes `.slimfit/baseline.json` from that measurement, keeping the verdicts already recorded there: its loosenings, `mutants_from`, the files kept, and each test kept while its text is unchanged. Code already on the branch changes land on is never charged to a mutants run: only the source lines a change adds after it are (see [what the gate holds](help:mutants-scope)). It adds `.slimfit/out` to `.gitignore`, and keeps the measurement and each suite's reports and logs in `.slimfit/out/`. With `--dry-run` it writes none of these (see [dry runs](help:dry-run)).
    ///
    /// ## What it answers
    ///
    /// - `config`: `"written"` or `"kept"`.
    /// - `suites`: each suite's settings, the preset filled in: `name`, `preset`, `root` (its directory in the repo), `sources`, `tests` and `excludes` (globs of source, test and ignored files), `inline_tests` (whether tests sit inside source files), `build_command` (run first, so the tests' CPU time leaves the build out; empty when the test command builds for itself), `test_command`, `test_args` (arguments the Rust preset appends to nextest's test run and to each mutant's, such as a profile and a filterset; empty when there are none: see below), `tests_report` and `coverage_report` (each a `format` and a `path`), `reach_report` (where the test run records which functions and lines each test ran; an empty `path` when it does not), `mutants_command`, `mutants_report`, `mutants_diff_flag` (the flag that limits the mutation tool to the lines of a diff; empty when it has none), `mutants_pick_flag` (the flag that runs one mutant by name; empty when it has none) and `mutants_tests` ("reached": a mutant runs only the tests that ran its function, see [mutants scope](help:mutants-scope); "all": every test).
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
    /// - A Rust suite's tests run as the repo's own gate runs them only with its `test_args`: in the shell's syntax, expanded each time a command runs, so a list kept in a file is read from it then and lives nowhere else, e.g. `test_args = '--profile fast -E "not ($(cat .config/slow-tests))"'`. The JUnit report is then read from that profile's directory (`target/nextest/fast/`). Only the Rust preset reads them (as `$SLIMFIT_TEST_ARGS`, which a suite's own `test_command` may read too); no `test_command` of the repo's own is needed for this. The test run runs every test whatever fails (`--no-fail-fast`), as coverage and which tests run each function must be of every test; `test_args` that say when a failure stops the run (`--fail-fast`, `--max-fail`) are passed instead, and a run they leave tests out of is refused, naming them. Each mutant's test run still stops at its first failure.
    #[command(next_help_heading = "Run once, to adopt slimfit on a repo", verbatim_doc_comment)]
    Init {
        #[command(flatten)]
        at: At,
        /// Measure, and answer what it would write in `would_write`; write nothing (see [dry runs](help:dry-run)).
        #[arg(long, verbatim_doc_comment)]
        dry_run: bool,
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
    /// - `notes`: what was not measured and what that hides: mutants not run, or a test run that records no coverage per test (redundant-test tasks need it; the test runs of {const:PER_TEST} record it).
    /// - `next`: what to do now.
    ///
    /// A task has `kind`, `suite`, `summary`, `files` (the files to work on), `evidence` (the numbers that make it a task), `steps` (what to do) and `accept` (the command that must pass after). Its `evidence`, by kind:
    ///
    /// - `failing-test`: `test` and `log`, the test run's log.
    /// - `pick-mutants`: `lines`, the source lines the change adds, and `candidates`, the change's mutants to pick from: {const:CANDIDATE}.
    /// - `justify-growth`: `lines`, the source lines the change adds net; `allowance`, `growth_allowance`; `from`, where the change starts (`git diff <from>` is the diff the reviewer is given); and `grown`, each file whose source lines changed, with its `lines` (net, negative for a file that shrank), most first.
    /// - `uncovered`: `file`, `start`, `end` and `lines` of the span no test runs, and `function`, when known.
    /// - `redundant-test`: `test`, its `seconds`, `lines` (how many source lines it runs) and `other_test`, the test whose lines it overlaps most: one that runs every one of them too, when there is one.
    /// - `slow-test`: `test`, its `seconds`, and `suite_seconds`, the suite's `test_seconds`.
    /// - `surviving-mutant`: `source`, who chose the mutants (`picker`: picks of the change, by a second agent; `sampled`: drawn by `--sample`), and `survivors`, each that survived, with its `line` and `mutant` (the change it makes), and a sampled one its `name`.
    /// - `record-sample`: `path`, where the sample's record goes, `counts` (its mutants by outcome) and `text`, the record to write there.
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
    /// - Surviving-mutant tasks name only picks that survived, and with `--sample` the sample's survivors: nothing runs the whole codebase's mutants (see [mutants scope](help:mutants-scope)).
    /// - `--sample` runs mutants, unlike the rest of `next`: put it on the idle tasks line only, never in the loop or a gate. No `check` reads what it finds (see [the sample](help:sample)).
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
        /// Also draw and run an unbiased sample of N mutants of the changes landed since the last sample, out of the loop: on factorium's idle tasks line, never in a gate or a worker's loop (see [the sample](help:sample)).
        #[arg(long, value_name = "N", verbatim_doc_comment)]
        sample: Option<usize>,
        /// Measure again even if the working tree is unchanged.
        #[arg(long, verbatim_doc_comment)]
        fresh: bool,
    },
    /// Fails if any bound is worse than the baseline; exits {const:EXIT_VIOLATION} on a violation.
    ///
    /// ## When to run it
    ///
    /// After each change, and as a task's `accept` command. In a gate, in place of the project's own test run: `slimfit check` IS that test run, instrumented (see [setting up in factorium](help:factorium)). `--mutants` is run by the agent on its own change: first with no name, to list the mutants a second agent picks from (see [the picker](help:picker)); then with its picks, at background priority (in factorium, `factorium worker exclusive --background -- slimfit check --mutants <name>... --why <why>...`), before `slimfit ratchet`; never in a gate, not as idle work. The plain `check` in the gate holds that the change adds a record of them: `unrecorded_mutants` (see [what the gate holds](help:mutants-scope)); and, when it adds more than `growth_allowance` source lines net, a growth record: `unrecorded_growth` (see [the reviewer](help:reviewer)).
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml`, `.slimfit/baseline.json`, which files in `.slimfit/mutants/` and `.slimfit/growth/` the change adds, and the files the change touches as they were where it starts. It measures the working tree, or reuses the measurement in `.slimfit/out/` when the tree is unchanged since it was taken, and writes nothing but that cache, the suites' reports and logs beside it, and with `--mutants` each pick's first outcome since the last record (`.slimfit/out/first-outcomes.json`, read by `slimfit ratchet`). The baseline is never written: `slimfit ratchet` does that.
    ///
    /// ## What it holds
    ///
    /// Per suite, against `.slimfit/baseline.json`, and the change's records in `.slimfit/mutants/` and `.slimfit/growth/`:
    ///
    /// {table:bound}
    ///
    /// It fails only on `failures`, `survived_mutants`, `unrecorded_mutants` and `unrecorded_growth`. `test_seconds` and `uncovered_lines` are trends: answered against the baseline and ratcheted down, never a failure; their tasks (`slow-test`, `uncovered`) and `slimfit report` are how they improve. Line counts are recorded and reported, not bounded: a change's growth past `growth_allowance` asks for a growth record (`unrecorded_growth`), and the record is what is held, never the number. To raise a bound, see `slimfit ratchet --help`.
    ///
    /// ## What it answers
    ///
    /// - `verdict`: `ok`, true when no bound is violated; `violations`, {const:VIOLATION}; `improved`, the bounds that got better (`slimfit ratchet` locks them in), each with `suite`, `metric`, `baseline` and `current`; `new_suites`, suites with no baseline yet; `mutants_unchecked`, true when mutants were not measured; `time_unchecked`, the suites whose `test_seconds` was not measured as a gate does: their tests ran inside `--mutants` (see [host load](help:host-load)); `test_seconds`, each timed suite's against the baseline, {const:TREND}: a trend, which never fails the check; and `uncovered_lines`, each suite's against the baseline, likewise, and {const:NEW_SPANS}: what the change leaves untested, for you to judge, never a failure.
    /// - `landing`: the change the gate holds to a record of its mutants (see [what the gate holds](help:mutants-scope)) and of its growth (see [the reviewer](help:reviewer)): {const:LANDING}; `null` when there is no branch it lands on (`slimfit doctor` finds `no_target`).
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
    /// After a task's `accept` command passes, then commit `.slimfit/baseline.json` with the change it records (one task, one commit). On a change that adds source lines, run a second agent's picks of its mutants first (see [the picker](help:picker)), `slimfit check --mutants <name>... --why <why>...` at background priority (in factorium, `factorium worker exclusive --background -- slimfit check --mutants ...`): never in a gate, not as idle work. After it passes, it reuses that measurement and writes the change's record in a file of its own in `.slimfit/mutants/`: commit it with the change, or the gate's `check` fails (see [what the gate holds](help:mutants-scope)). When the picker finds nothing worth a mutant, `--no-mutants "<its why>"` writes a record of that instead. Either record names who picked and who made the change (`--picker`, `--author`). A survivor that changes nothing observable is accepted with `--loosen survived_mutants --reason "<why>"`. On a change that adds more than `growth_allowance` source lines net, after a second agent judged whether it could be smaller (see [the reviewer](help:reviewer)), `--serves`, `--considered`, `--as-is` (or `--cut` with `--answer`) and `--reviewer` write its growth record: commit it with the change, or the gate's `check` fails.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml` and `.slimfit/baseline.json`, measures (or reuses the measurement in `.slimfit/out/` when the tree is unchanged), and writes `.slimfit/baseline.json`: each bound at the better of baseline and now, a loosened bound at now, with its reason, and beside the bounds `measured`, per suite the `uncovered_lines` and `test_seconds` measured now (`slimfit report` shows them landing by landing, and `slimfit next` gives a run of rises as a `rising-trend` task). When the measurement ran picked mutants, or with `--no-mutants`, it also writes `.slimfit/mutants/<hash>.json`, the change's record: the tree it was made of, `picker` and `author` (the ids given), and per suite the picks ({const:PICK}), or `none`, the reason none was picked; named by its contents; each pick's `first_outcome` is its outcome on the first `check --mutants` that ran it since the last record, kept in `.slimfit/out/` until this record is written, so a survivor killed before the record still counts as one (see [what the gate holds](help:mutants-scope)). With any growth flag, it also writes `.slimfit/growth/<hash>.json`, the growth record: the tree, `lines` (the source lines the change adds net now), `serves`, `considered`, `reviewer`, `author`, and `as_is`, or `cut` and `answer`; named by its contents (see [the reviewer](help:reviewer)). With `--dry-run` it writes nothing, and caches no measurement (see [dry runs](help:dry-run)).
    ///
    /// ## What it answers
    ///
    /// - `changed`: whether the baseline moved. `false` is nothing to ratchet: an answer (exit {const:EXIT_OK}), and the baseline is not rewritten.
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `loosened`: every loosening kept in the baseline, {const:LOOSENING}.
    /// - `record`: after `--mutants <name>...` or with `--no-mutants`, the change's record, `.slimfit/mutants/<hash>.json`; `null` when there is none.
    /// - `growth_record`: with a growth flag, the growth record, `.slimfit/growth/<hash>.json`; `null` when there is none.
    /// - `next`: commit, then `slimfit next` (or, when nothing changed, just `slimfit next`).
    /// - `would_write`: with `--dry-run` only, each file it would change (`.slimfit/baseline.json`, the record), with its whole new text.
    ///
    /// ## What it may refuse
    ///
    /// It writes nothing when it refuses.
    ///
    /// - A bound got worse: `failures` and `unrecorded_mutants` always; any other unless `--loosen` names it.
    /// - `--loosen` without `--reason`; `--loosen failures` (make them pass instead), `--loosen unrecorded_mutants` (pick and record the mutants instead) and `--loosen unrecorded_growth` (record the review instead).
    /// - A growth record without `--serves`, `--considered`, `--reviewer`, `--author` and a verdict (`--as-is`, or `--cut` with `--answer`), each one line; or with `--reviewer` equal to `--author`: a second agent judges (see [the reviewer](help:reviewer)).
    /// - `--no-mutants` without a one-line reason, or of a measurement that ran picks (their record is the one).
    /// - A record without `--picker` and `--author` (one line each), or with the two equal: a second agent picks (see [the picker](help:picker)); and either flag with no record to write.
    /// - {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - It runs no mutant, and takes no `--mutants`: mutants run one way, `slimfit check --mutants` at background priority, and `ratchet` reuses that measurement while the tree is unchanged. A second path would run them at the agent's own priority.
    /// - When the tests ran inside `--mutants` and no gate's run timed the same files, it runs the tests again, at your priority, to time them (see [host load](help:host-load)): run it as you run a gate, not at background priority.
    /// - A `ratchet` of a measurement without picks, and without `--no-mutants`, writes no record: the gate's `check` fails on a change that adds source lines until one is committed.
    /// - The two records are separate: one `ratchet` may write both, with one `--author`, or each on its own.
    /// - {const:CACHE_PITFALL}
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Ratchet {
        #[command(flatten)]
        at: At,
        /// Allow this bound to rise (repeatable); needs `--reason`. Only `survived_mutants` rises so: the others are refused, `failures`, `unrecorded_mutants` and `unrecorded_growth` as never loosened, `test_seconds` and `uncovered_lines` as trends, with nothing to loosen.
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
        /// Record that nothing in the change is worth a mutant, and why (one line): the picker's answer when it picks none. No mutant runs.
        #[arg(long, value_name = "WHY", verbatim_doc_comment)]
        no_mutants: Option<String>,
        /// Who picked the mutants this record holds (or said none is worth one): the second agent, by an id its caller gives, such as its session id with harness and model; kept in the record (see [the picker](help:picker)).
        #[arg(long, value_name = "ID", verbatim_doc_comment)]
        picker: Option<String>,
        /// Who made the change, by an id given as `--picker`'s is; kept in the record (and in the growth record). It must differ from `--picker` (and from `--reviewer`).
        #[arg(long, value_name = "ID", verbatim_doc_comment)]
        author: Option<String>,
        #[command(flatten)]
        growth: Box<GrowthArgs>,
    },
    /// Records that a candidate is kept as it is: a redundant test, or a file a deepen task proposed. It is not proposed again until it changes as below.
    ///
    /// ## When to run it
    ///
    /// When a `redundant-test` or `deepen` task's steps end in KEEP: the assertions showed the test checks what the other cannot take over, or the module is already deep enough and nothing is worth changing. Then commit `.slimfit/baseline.json`.
    ///
    /// ## What it reads and writes
    ///
    /// It reads `slimfit.toml` and `.slimfit/baseline.json`. A path to one of a suite's source files is a file kept: it reads the file's prod lines and its git history, and measures nothing. Anything else is a test: it reads the measurement (from the cache in `.slimfit/out/` when the tree is unchanged, else it measures as `slimfit check` does) for the test's own function: where it is and a hash of its text, from its `fn` line to its closing brace. It writes `.slimfit/baseline.json`'s `kept` (the test, that hash and the reason) or `kept_files` (the file, its size and churn now, and the reason), replacing an earlier verdict on the same test or file.
    ///
    /// When it is proposed again:
    ///
    /// - A test: when its text changes. `slimfit ratchet` drops a verdict whose test changed or is gone.
    /// - A file: once its prod lines grow by {const:KEPT_GROWTH} of `prod_lines`, or {const:KEPT_CHURN} × `commits_90d` commits more change it (counted against `commits_total`), and it is still a deepen candidate. Keeping it again records its size and churn then.
    ///
    /// ## What it answers
    ///
    /// - `kept`: every test kept in the baseline, {const:KEPT}.
    /// - `kept_files`: every file kept in the baseline, {const:KEPT_FILE}.
    /// - `next`: commit `.slimfit/baseline.json`, then `slimfit next`.
    ///
    /// ## What it may refuse
    ///
    /// It writes nothing when it refuses.
    ///
    /// - No `--reason`.
    /// - No suite's test run recorded the test (its `test` id is a redundant-test task's `test`), and no suite counts it as a source file.
    /// - Its own function was not found, so there is no text to hold the verdict to.
    /// - {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - A test's verdict holds against the test's text only: a change to the other test, or to the code both run, does not bring it back. Any edit inside the test does, a reformat included.
    /// - A file's verdict holds against its size and churn only: a rewrite that keeps both under the thresholds does not bring it back.
    #[command(next_help_heading = "Run by the agent, in the loop", verbatim_doc_comment)]
    Keep {
        #[command(flatten)]
        at: At,
        /// The test kept (a redundant-test task's `test`) or the file kept (a deepen task's `file`).
        #[arg(verbatim_doc_comment)]
        test: String,
        /// Why it stays: what the test checks that the other does not, or why the module is deep enough; kept in the baseline.
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
    /// It reads `slimfit.toml`, `.slimfit/baseline.json`, the records in `.slimfit/mutants/` and `.slimfit/growth/`, the samples in `.slimfit/samples/`, and the source and test files at commits spread over the first-parent history. It measures as `slimfit check` does, without mutants (from the cache in `.slimfit/out/` when the tree is unchanged), and writes nothing else. With `--lines-only` it measures nothing: it reads HEAD's files from git, in seconds.
    ///
    /// ## What it answers
    ///
    /// - `current`: per suite, the metrics measured now: {const:METRICS}. With `--lines-only`, only `prod_lines` and `test_lines`, of HEAD.
    /// - `by` (with `--by`): per directory a glob matches, per suite, the `prod_lines` and `test_lines` of the files in it at HEAD, counted as `current`'s are (the same files, the same blank and `//` rule, a test crate's files as test). A directory with no file a suite counts is absent; one inside another match counts in both. The directories' lines sum to `current`'s when every counted file is in exactly one.
    /// - `baseline`: per suite, {const:METRICS}.
    /// - `loosened`: every loosening kept in the baseline, {const:LOOSENING}.
    /// - `history`: oldest first, each point's `commit`, `date`, `suites` (per suite, {const:HISTORY_SUITE}), `by` (with `--by`: as above, at that commit), `mutants_records` (the records `.slimfit/mutants/` held then, counted as below), `growth_records` (the growth records `.slimfit/growth/` held then, by verdict, as below; absent when none) and `mutants_survival` (per source of the mutants that ran by then, by who picked: `picker` for a second agent's picks in records, `author` for picks in records that name no other picker, and `sampled` for the samples in `.slimfit/samples/`: {const:RATE}; see [the sample](help:sample)). Read from git, no test run: whether the agents' judgments move the codebase the right way shows as a trend here, not in any one verdict. The trend: `measured` shows each landing's `uncovered_lines` and `test_seconds` as measured, so a run of rises the ratcheted numbers hide shows here (and as a `rising-trend` task of `slimfit next`). The tally: `first_survived` per source is how many gaps its picks found, survivors since killed included, where `survived` counts only those still alive when recorded; a `picker` count above the `author` one is what a second agent buys. A number from before the baseline held it (or before any record) is absent, not zero.
    /// - `mutants_records`: how many records `.slimfit/mutants/` holds: {const:RECORDS}. Many without picks, or picks that are all trivial, is a rule kept in form only; so is a `picker` rate far below the `sampled` one in `mutants_survival`.
    /// - `growth_records`: how many growth records `.slimfit/growth/` holds: {const:GROWTH_RECORDS}. Beside `history`'s `prod` lines: lines that keep climbing while every record says `as_is` is a review kept in form only (see [the reviewer](help:reviewer)).
    /// - `next`: `slimfit next`.
    ///
    /// ## What it may refuse
    ///
    /// {const:MEASURE_REFUSALS}
    ///
    /// ## What trips you up
    ///
    /// - `history` counts lines with today's `slimfit.toml` (and a Rust suite's test crates by today's Cargo manifests): a commit from before a suite existed counts it as it is configured now.
    /// - `history`'s `tests`, `test_seconds` and `uncovered_lines` are what the baseline held at that commit, not a measurement of it: for `test_seconds` and `uncovered_lines`, the lowest ratcheted so far; a rise shows in `measured` beside them, which baselines written before it was kept lack; `test_seconds` changed how it is measured over time (load-corrected, then CPU time), so a step in it can be the measure, not the code.
    /// - `by` counts HEAD, not the working tree: without `--lines-only`, `current` is the working tree's measurement, so the two differ by what is not committed.
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
        #[arg(long, conflicts_with = "lines_only", verbatim_doc_comment)]
        fresh: bool,
        /// Also count each suite's lines per directory the glob matches, relative to the repo's root (a `*` does not cross a `/`), e.g. `--by 'crates/*'`; repeatable.
        #[arg(long, value_name = "GLOB", verbatim_doc_comment)]
        by: Vec<String>,
        /// Count lines only, from git at HEAD: no test run, so `current` holds only `prod_lines` and `test_lines`.
        #[arg(long, verbatim_doc_comment)]
        lines_only: bool,
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

/// The growth record `ratchet` writes: any of these flags asks for one.
#[derive(clap::Args, Default)]
pub(crate) struct GrowthArgs {
    /// The issue the change's growth serves (one line); kept in the growth record (see [the reviewer](help:reviewer)).
    #[arg(long, value_name = "ISSUE", requires_all = ["considered", "reviewer", "verdict"], verbatim_doc_comment)]
    pub serves: Option<String>,
    /// What existing code you weighed reusing or deleting instead of adding (one line); kept in the growth record.
    #[arg(long, value_name = "WHAT", requires = "serves", verbatim_doc_comment)]
    pub considered: Option<String>,
    /// The reviewer's verdict that the change cannot be smaller, with its why (one line).
    #[arg(long, value_name = "WHY", group = "verdict", requires = "serves", verbatim_doc_comment)]
    pub as_is: Option<String>,
    /// What the reviewer named to cut (one line); needs `--answer`.
    #[arg(long, value_name = "WHAT", group = "verdict", requires_all = ["answer", "serves"], verbatim_doc_comment)]
    pub cut: Option<String>,
    /// What you did about each cut `--cut` names, or why not (one line).
    #[arg(long, value_name = "WHAT", requires = "cut", verbatim_doc_comment)]
    pub answer: Option<String>,
    /// Who judged the growth: the second agent, by an id its caller gives, as `--picker`'s is; kept in the growth record. It must differ from `--author`.
    #[arg(long, value_name = "ID", requires = "serves", verbatim_doc_comment)]
    pub reviewer: Option<String>,
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

#[doc = include_str!("help/picker.md")]
#[derive(Topic)]
#[topic(name = "picker")]
struct PickerTopic;

#[doc = include_str!("help/reviewer.md")]
#[derive(Topic)]
#[topic(name = "reviewer")]
struct ReviewerTopic;

#[doc = include_str!("help/sample.md")]
#[derive(Topic)]
#[topic(name = "sample")]
struct SampleTopic;

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
        .topic::<PickerTopic>()
        .topic::<ReviewerTopic>()
        .topic::<SampleTopic>()
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
        .constant("BUSY_LOAD", doctor::BUSY)
        .constant("COVERAGE_SLACK", check.coverage_slack)
        .constant("GROWTH_ALLOWANCE", check.growth_allowance)
        .constant("GROWTH_RECORDS", growth::RECORDS_HELP)
        .constant("LISTED_PER_KIND", tasks::LISTED_PER_KIND)
        .constant("LISTED_DEEPEN", tasks::LISTED_DEEPEN)
        .constant("DEEPEN_DAYS", tasks::DEEPEN_DAYS)
        .constant("TREND_LANDINGS", tasks::TREND_LANDINGS)
        .constant("TREND_MIN", tasks::TREND_MIN)
        .constant("TIME_NOISE", format!("{:.0}%", tasks::TIME_NOISE * 100.0))
        .constant("COMMON_LINES", format!("{:.0}%", tasks::COMMON_LINES * 100.0))
        .constant("METRICS", model::METRICS_HELP)
        .constant("VIOLATION", ratchet::VIOLATION_HELP)
        .constant("TREND", ratchet::TREND_HELP)
        .constant("NEW_SPANS", ratchet::NEW_SPANS_HELP)
        .constant("LANDING", landing::HELD_HELP)
        .constant("RECORDS", landing::RECORDS_HELP)
        .constant("HISTORY_SUITE", report::LINES_HELP)
        .constant("RATE", report::RATE_HELP)
        .constant("SAMPLE", measure::sample::SAMPLE_HELP)
        .constant("SAMPLE_DAYS", measure::sample::SAMPLE_DAYS)
        .constant("SAMPLE_DRAWN", measure::sample::DRAWN)
        .constant("SAMPLE_COST", measure::sample::COST)
        .constant("CANDIDATE", model::CANDIDATE_HELP)
        .constant("PICK", model::PICK_HELP)
        .constant("LOOSENING", ratchet::LOOSENING_HELP)
        .constant("KEPT", ratchet::KEPT_HELP)
        .constant("KEPT_FILE", ratchet::KEPT_FILE_HELP)
        .constant("KEPT_GROWTH", format!("{:.0}%", ratchet::KEPT_GROWTH * 100.0))
        .constant("KEPT_CHURN", ratchet::KEPT_CHURN)
        .constant("MEASURE_REFUSALS", measure::refusals_help())
        .constant("CACHE_PITFALL", measure::CACHE_HELP)
        .constant("RUST_TESTS", prose_list(config::RUST_TESTS.iter().map(|g| format!("`{g}`")), "and"))
}

/// What an answer exits with, nothing left to do included.
pub(crate) const EXIT_OK: i32 = 0;
/// What a command that cannot finish exits with.
pub(crate) const EXIT_ERROR: i32 = 2;
/// What `check` exits with when a bound is violated.
pub(crate) const EXIT_VIOLATION: i32 = 1;

/// `items` as prose: `a`, `a and b`, `a, b and c`, with `conjunction` for "and".
fn prose_list(items: impl IntoIterator<Item = String>, conjunction: &str) -> String {
    let mut items: Vec<String> = items.into_iter().collect();
    let last = items.pop().unwrap_or_default();
    if items.is_empty() { last } else { format!("{} {conjunction} {last}", items.join(", ")) }
}

/// The languages slimfit has a preset for.
pub(crate) fn languages(conjunction: &str) -> String {
    prose_list(config::Preset::all().iter().map(|p| p.language().to_string()), conjunction)
}

/// The languages whose preset has `property`.
fn languages_where(property: impl Fn(&config::Suite) -> bool) -> String {
    let presets = config::Preset::all().iter().filter(|p| property(&config::preset(**p)));
    prose_list(presets.map(|p| p.language().to_string()), "and")
}

/// The command line: the repository it names (`--repo`) and its command.
/// `--help`, `-h` and `help [<topic>]` print the manual's answer and exit
/// here, as clap does; a line clap refuses, or one asking for what is gone,
/// is a fault with its fix.
pub(crate) fn read(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<(Option<PathBuf>, Cmd)> {
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
    Ok((at.repo.clone(), cli.command))
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
        let held = crate::landing::Held::default();
        assert_eq!(named(crate::landing::HELD_HELP), keys(&held));
        assert_eq!(named(crate::landing::RECORDS_HELP), keys(&crate::landing::Records::default()));
        assert_eq!(named(crate::report::RATE_HELP), keys(&crate::report::Rate::default()));
        assert_eq!(
            named(crate::measure::sample::SAMPLE_HELP)[..4],
            keys(&crate::measure::sample::Sample::default())[..]
        );
        let candidate = model::Candidate::default();
        assert_eq!(named(model::CANDIDATE_HELP), keys(&candidate));
        let pick = model::Pick { name: String::new(), why: String::new(), outcome: None, first_outcome: None };
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
