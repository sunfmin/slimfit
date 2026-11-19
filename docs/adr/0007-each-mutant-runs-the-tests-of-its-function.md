# 7. Each mutant runs only the tests that ran its function, each under its own timeout

## Context

ADR 0006 bounded a mutants run by the change, but each mutant still ran
the whole suite under cargo-mutants' suite-wide timeout, after an unmutated
baseline run. The control session measured the change of factorium#107
(factorium c0dc2e5: `idle.rs` + `lib.rs`, 25+/10- lines, 17 changed-line
mutants, `-j1`, host load 14–28, 2026-11-19). Every configuration gave
the same verdicts, 5 caught, 12 unviable, 0 missed:

| run | wall | where the time went |
|---|---|---|
| A: workspace tests, fail-fast, cargo-mutants' own timeout | 274s | baseline 37s; 3 mutants that hang waited the auto timeout, 71s each (213s, 78%); 2 caught at ~6s |
| B1: + nextest `slow-timeout` (15s per test), `--baseline skip` | 96s | the 3 hangs end in 17.6s each |
| B2: + only the 6 of 213 tests that reach the mutated function, per-test timeout 9s | 69s | hangs 9.2s, caught 1.2s, unviable 23s, first build 9.8s |

## Decision

- **The test run records which functions each test ran.** The Rust
  preset's test command (`src/presets/rust-test.sh`) gives nextest a
  target runner (`--config target.'cfg(all())'.runner`) that sets, per
  test, an `LLVM_PROFILE_FILE` of its own beside cargo llvm-cov's. Every
  process the test starts inherits it, so spawned binaries count. cargo
  llvm-cov merges every profile of its directory, so the merged coverage is
  unchanged (on the Rust sample, the LCOV of a run with and without the
  runner differs in no `DA`, `FNDA` or `LF`/`LH` line). After the run,
  each test's profiles are read into `reach.txt` (`reach_report`). As first
  built, by `llvm-profdata show --all-functions --counts`, a function ran
  when any of its counters was above 0; since slimfit#32 (ADR 0006), by
  `llvm-cov export -format=lcov` of the test's merged profiles (format
  `lcov`), which gives its lines too: a function ran when its `FNDA` count
  is above 0. Both name functions by symbol, which the LCOV report's `FN`
  records map to a file and a first line (`Function.symbol`).
- **One per-test record.** The measurement keeps it as `reach`: the tests
  (nextest's binary id and name), and per source file, per function's first
  line, the tests that ran it. Every function of the coverage report is
  there, those no test ran with none. `reach.lines`, the lines each test
  ran, replaces ADR 0006's `test_lines` in the same record; slimfit#32
  fills it (ADR 0006, Built).
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
  and each mutant's log say which.
- **Each test under its own timeout.** The shim gives each test it runs
  nextest's `slow-timeout = { period = T, terminate-after = 1 }`, T =
  `TIMEOUT_FLOOR` 5s + `TIMEOUT_FACTOR` 5 × its duration in the test run
  (JUnit `time`, the load taken out, ADR 0003). nextest ends a test that
  runs past it and reports it failed (`TIMEOUT`), so the mutant is caught,
  as cargo-mutants counts its own timeout.
- **No baseline run.** The preset passes `--baseline skip`: slimfit's own
  test run of this tree already showed it green, and a cached one is
  reused. cargo-mutants' suite-wide timeout is then its 300s fallback, a
  backstop only.
- `mutants_tests = "all"` in a suite turns it off: every test on every
  mutant, as before.

The shim waits for the real nextest rather than `exec` it: a process that
execs never writes its own coverage profile, and slimfit measures its own
coverage through these processes too.

## Kill matrix

The brief asked that redundant-test tasks still come from complete killer
lists among the selected tests. Since ADR 0006 there is no kill matrix:
redundant tests come from per-test coverage, which `reach` now holds. A
selected run with fail-fast is all the verdicts need.

## Consequences

- A mutant costs its build plus the tests that ran its function, at most
  the whole suite; one that hangs costs its test's timeout, not the
  suite's (see `slimfit help costs`).
- A mutated function that nearly every test runs (factorium's daemon tick)
  gains nothing from the selection: its cost is B1's.
- The Rust test run writes one profile per test process: more files and
  one read of them per test after the run (Measured; since slimfit#32 an
  `llvm-profdata merge` and an `llvm-cov export` per test, as many at once
  as there are cores: ADR 0006, Measured).
- A repository with a target runner of its own: `--config` on the command
  line takes precedence for `cfg(all())`, but a runner set for its target
  triple wins over it; then no test writes its own profile, no reach is
  recorded, and every mutant runs every test.
- The unviable mutants (cargo-mutants' body replacements for types with
  no default) still cost a build each; not in scope.

## Measured

`slimfit check --mutants` (this ADR's build, cargo-mutants 27.1.0, nextest
0.9.151, `-j1`) on a fresh clone of factorium at c0dc2e5, the baseline's
`mutants_at` at its first parent: the change of factorium#107 (`idle.rs` +
`lib.rs`, 25+/10- lines in `src`), 17 mutants (`--in-diff`). Both runs under
factorium's `worker exclusive --background`, 2026-11-19, one after the
other on a shared 18-core host.

| | each mutant runs the tests that ran its function | every test (`mutants_tests = "all"`, cargo-mutants `--timeout 60`) |
|---|---|---|
| 1-minute load | 4–8 | 5–20 |
| verdicts | 5 caught, 12 unviable, 0 missed | the same, mutant by mutant |
| tests per caught mutant | 6 of 213 (the same 6 as B2) | 213, fail-fast |
| a hanging mutant (3 of them) | 20.5–23.5s: its test's own timeout, 5s + 5 × ~3.4s | 60s each: the suite-wide timeout (71s with cargo-mutants' own, run A) |
| the other 2 caught | 3.5s each | 29s and 36s |
| the 12 unviable | the first 98s (the first build of cargo-mutants' scratch copy of the tree, which has no `target`), then 1–2.6s each | the same |
| mutants, wall | 227s | 399s |
| `check --mutants`, wall | 413s (test run 184s: a fresh clone, so a cold instrumented build) | 518s (test run 118s, warm) |

Recording which functions each test ran cost 14s of the test run (from
`lcov.info` written to `reach.txt` written: 327 profiles of 213 tests).
`reach` adds 1.1 MB to `.slimfit/out/measurement.json`.

So against run A of the Context (274s, with its 37s baseline), the
mutants of this change took 227s, and 98s of that is the cold first build
of the scratch copy, which the Context's B2 (9.8s) did not pay. Without
it, 129s. The rest that remains over B2 is the hangs: 3 × ~22s against
B2's flat 9s. `TIMEOUT_FACTOR` trades that for margin: a test slowed by
the host's load or by a mutant that does not hang must not be ended. A
first build that is not cold (a scratch copy that keeps `target`) is the
next lever. Not measured: a function that nearly every test runs (the
daemon's tick), which costs B1's.

## The first build (slimfit#35)

The lever named above, measured as above: `slimfit check --mutants` on a
fresh clone of factorium at c0dc2e5, `mutants_at` at its first parent
(ed1ff43), cargo-mutants 27.1.0, cargo-llvm-cov 0.9.1, `-j1`, each run
under `worker exclusive --background`, 2026-11-19, one after the other on
the same clone (the 1-minute load 5–22). Each variant was set as the
suite's `mutants_command` in `slimfit.toml`; the time is cargo-mutants'
phases (`outcomes.json`), its first build that of its first mutant.

| variant | first build | crates it compiled | mutants |
|---|---|---|---|
| A: the preset as it was | 106.2s | 69 | 226.1s |
| `--copy-target=true` | 94.0s | 69 | 222.2s |
| `--copy-target=true`, `CARGO_TARGET_DIR=target/llvm-cov-target` (the copy's) | 99.8s | 36 | 223.0s |
| the same, in the test run's environment (`cargo llvm-cov show-env`) | 95.6s | 36 | 205.1s |
| `--in-place` | 114.8s | 69 | 335.4s |
| a target directory of its own at a fixed path, first run | 110.6s | 69 | 230.9s |
| the same, second run | 13.5s | 4 | 142.9s |
| B: the test run's environment and its own target directory | 11.1s | 4 | 136.3s |
| B as the preset now builds it (no `mutants_command` of the suite's own) | 8.8s | 4 | 129.2s |

Every run: 5 caught, 12 unviable, 0 missed, the same verdict mutant by
mutant, each caught one run on the same 6 of 213 tests (`--in-place`: on
every test, below).

- **Why copying `target` gains nothing.** A target directory at a new
  path recompiles every crate with a build script (its outputs name the
  old path) and every crate above one: proc-macro2, serde, libsqlite3-sys
  and 33 more, nearly all of the build. cargo-mutants' copy is a new
  temporary directory each run, so no copy of `target` stays warm.
- **Why not `--in-place`.** The tree's own `target/debug` is as cold
  (nothing but the instrumented test run built it), and the shim finds a
  mutant by comparing the build directory with the suite's own files:
  in place they are the same files, so every mutant ran every test.
  It also leaves a mutation in the working tree if the run is killed.
- **Decision: B.** The preset's `mutants_command` reads `cargo llvm-cov
  show-env --sh` (its rustc wrapper, flags and target directory) and
  builds into `$CARGO_LLVM_COV_TARGET_DIR/llvm-cov-target`, the directory
  `cargo llvm-cov nextest` built the test run in, at the same path and
  with the same flags: only the workspace crates the mutant's copy names
  anew compile. Each mutant's tests write their profiles into the copy
  (`LLVM_PROFILE_FILE` relative), which cargo-mutants deletes. When
  `show-env` gives nothing, the build is cold, as before.
- **What it costs.** The mutants' builds leave the test run's directory
  holding the workspace crates of the copy: the next test run recompiles
  them (35s at background priority on factorium) and measures the same
  coverage (4959 covered, 168 uncovered, 213 tests passed). An edit after
  the run recompiles them anyway. Not measured: cargo-mutants with
  `--jobs` above 1, whose copies would share that one directory, so one
  mutant's test binary could be another's. The preset does not pass it; a
  suite that adds it must drop `CARGO_TARGET_DIR` from its command.

So the mutants of this change take 129-136s instead of 226s: of A's
226s, the 95-97s that was the cold build is gone; what is left over B2 of the
Context (69s) is the hangs' timeouts and the first mutant that compiles
(about 20s, linking the test binaries).
