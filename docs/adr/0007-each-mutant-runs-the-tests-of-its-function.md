# 7. Each mutant runs only the tests that ran its function, each under its own timeout

## Context

A mutant run under cargo-mutants' defaults runs the whole suite after an
unmutated baseline run, under one suite-wide timeout. On factorium#107's
change (17 mutants) that took 274s, 78% of it three hanging mutants waiting
out the suite-wide timeout; with a per-test timeout, no baseline run, and
only the 6 of 213 tests that reach the mutated function, the same verdicts
took 69s (evidence: slimfit#40, comment "Evidence of slimfit#29").

## Decision

- **The test run records which functions each test ran.** The Rust
  preset's test command (`src/presets/rust-test.sh`) gives nextest a
  target runner (`--config target.'cfg(all())'.runner`) that sets, per
  test, an `LLVM_PROFILE_FILE` of its own beside cargo llvm-cov's. Every
  process the test starts inherits it, so spawned binaries count. cargo
  llvm-cov merges every profile of its directory, so the merged coverage is
  unchanged (on the Rust sample, the LCOV of a run with and without the
  runner differs in no `DA`, `FNDA` or `LF`/`LH` line). After the run, each
  test's merged profiles are exported by `llvm-cov export -format=lcov` into
  `reach.txt` (`reach_report`, format `lcov`): a function ran when its
  `FNDA` count is above 0. Functions are named by symbol, which the LCOV
  report's `FN` records map to a file and a first line (`Function.symbol`).
- **One per-test record.** The measurement keeps it as `reach`: the tests
  (nextest's binary id and name), and per source file, per function's first
  line, the tests that ran it. Every function of the coverage report is
  there, those no test ran with none. `reach.lines` holds the lines each
  test ran (ADR 0006).
- **Each mutant runs the tests of its function.** `check --mutants` first
  lists the change's mutants (`cargo mutants ... --list --json`, the same
  command and diff), and plans for each listed function (its span,
  attributes included) the tests that ran a function the coverage report
  starts within it: closures, nested functions and every instantiation
  of a generic are in its lines. cargo-mutants runs one test command for
  every mutant, so the choice is made where that command lands: slimfit
  links itself, first on PATH, as `cargo-nextest`. For each `cargo nextest
  run` (not `--no-run`, which passes through) the shim finds the mutant by
  the first line of the build directory that differs from the suite's own
  files, and runs the real nextest with `-E` naming the tests of the
  functions around that line. A mutant whose function no test ran runs no
  test: the shim exits 0, and the mutant, once built, is missed. The build
  still runs, so an unviable mutant stays unviable.
- **Why the verdicts stay those of every test.** A test that never runs
  the mutated function cannot fail on it. Where that is not known, every
  test runs: a line in no listed function, a function no coverage record
  starts in (a generic never instantiated, a build script), a `const fn`
  (evaluated while compiling, so no profile shows it), a test whose
  profile could not be read (counted as running every function), no file
  that differs (cargo-mutants' unmutated baseline), no `reach_report`
  written (a test command or target runner of the repo's own), no real
  `cargo-nextest` found. Never fewer tests than a mutant may need; stderr
  and each mutant's log say which. On factorium#107's change the verdicts
  matched every-test runs mutant by mutant.
- **Each test under its own timeout.** The shim gives each test it runs
  nextest's `slow-timeout = { period = T, terminate-after = 1 }`, T =
  `TIMEOUT_FLOOR` 5s + `TIMEOUT_FACTOR` 5 × its duration in the test run
  (JUnit `time`, the load taken out, ADR 0003). nextest ends a test that
  runs past it and reports it failed (`TIMEOUT`), so the mutant is caught,
  as cargo-mutants counts its own timeout. The factor is margin: a test
  slowed by the host's load, or by a mutant that does not hang, must not
  be ended.
- **No baseline run.** The preset passes `--baseline skip`: slimfit's own
  test run of this tree already showed it green, and a cached one is
  reused. cargo-mutants' suite-wide timeout is then its 300s fallback, a
  backstop only.
- **No cold first build.** The preset's `mutants_command` reads `cargo
  llvm-cov show-env --sh` (its rustc wrapper, flags and target directory)
  and builds into `$CARGO_LLVM_COV_TARGET_DIR/llvm-cov-target`, the
  directory `cargo llvm-cov nextest` built the test run in, at the same
  path and with the same flags: only the workspace crates the mutant's copy
  names anew compile. On factorium that took the first build from 106s to
  9-11s, and the change's mutants from 226s to 126-136s (evidence:
  slimfit#40, comment "Evidence of slimfit#35"). show-env's
  `LLVM_PROFILE_FILE` is left out: each mutant's tests keep the one they
  had (unset in an agent's shell, so their profiles land in the copy, which
  cargo-mutants deletes; in slimfit's own tests, the test's, so the shim's
  coverage still counts). When `show-env` gives nothing, the build is cold.
  Not taken: copying `target` (`--copy-target`), since a target directory
  at a new path recompiles every crate with a build script and every crate
  above one, and cargo-mutants' copy is a new directory each run; and
  `--in-place`, whose `target/debug` is as cold, where the shim cannot tell
  the mutant from the suite's files (so every mutant runs every test), and
  which leaves a mutation in the working tree if the run is killed.
- `mutants_tests = "all"` in a suite turns the selection off: every test on
  every mutant.

The shim waits for the real nextest rather than `exec` it: a process that
execs never writes its own coverage profile, and slimfit measures its own
coverage through these processes too.

## Consequences

- A mutant costs its build plus the tests that ran its function, at most
  the whole suite; one that hangs costs its test's timeout, not the
  suite's (see `slimfit help costs`).
- A mutated function that nearly every test runs (factorium's daemon tick)
  gains nothing from the selection: every test runs on it.
- The Rust test run writes one profile per test process, and after the run
  an `llvm-profdata merge` and an `llvm-cov export` per test, as many at
  once as there are cores (ADR 0006).
- A repository with a target runner of its own: `--config` on the command
  line takes precedence for `cfg(all())`, but a runner set for its target
  triple wins over it; then no test writes its own profile, no reach is
  recorded, and every mutant runs every test.
- The mutants' builds leave the test run's directory holding the workspace
  crates of the copy: the next test run recompiles them, and measures the
  same coverage. cargo-mutants with `--jobs` above 1 would share that one
  directory, so one mutant's test binary could be another's: the preset
  does not pass it, and a suite that adds it must drop `CARGO_TARGET_DIR`
  from its command.
- The unviable mutants (cargo-mutants' body replacements for types with
  no default) still cost a build each.
