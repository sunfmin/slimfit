# 4. Rust records every killing test: cargo-mutants under nextest, no fail-fast

**Superseded by ADR 0006** (`0006-mutants-only-on-a-change.md`): redundant tests come from per-test coverage, not a kill matrix; the Rust preset no longer passes `--no-fail-fast`, and no reader records killers.

## Context

`next` finds redundant tests from a kill matrix: for each killed mutant,
every test that failed on it (`tasks::redundant`). Stryker (`--disableBail`)
and PIT (`fullMutationMatrix`) write that matrix. cargo-mutants does not: its
`outcomes.json` says only whether a mutant was caught. It runs the suite
once per mutant and keeps that run's output in `mutants.out/log/`, but by
default it runs `cargo test`, which stops at the first failing test binary,
so the output names only some of the killers. factorium (Rust) is meant to
use every slimfit feature, so Rust needs the matrix.

## Decision

- The Rust preset runs `cargo mutants … --test-tool nextest
  --cargo-test-arg=--no-fail-fast`. nextest without fail-fast runs every test
  on every mutant and prints one line per test (`FAIL [ 0.010s] (2/4) calc
  tests::adds`), which names the test by the same `<binary-id>::<test>` its
  JUnit report (and so `slimfit`'s test ids) uses.
- The cargo-mutants reader follows each outcome's `log_path`, beside the
  `outcomes.json` it reads, and records as the mutant's killers every test
  the log shows failing. A mutant that timed out was stopped mid-run: the
  tests that started and never finished are its killers too. Every test the
  logs show starting (the baseline's log included) is a known test, so a test
  that kills nothing is found as well.
- The matrix is claimed only when it is complete: the outcome's test phase
  ran nextest with `--no-fail-fast` and its log is readable. If any killed
  mutant lacks that (a suite that overrides `mutants_command`, reports copied
  without their logs), no killer is recorded for any mutant, and `next` says
  the tool does not record killers. A partial matrix would make the tests it
  misses look redundant.
- Each run of the tool, whether over every file, the changed files (ADR 0001)
  or one pending file (ADR 0002), is read with its own logs right after it
  runs, so every run records its mutants' killers. `next` still gives
  redundant-test tasks only after a run over every file, as before: a run
  over some files cannot see what a test kills elsewhere.

## Cost

Measured on slimfit itself (cb0af32, cargo-mutants 27.1.0, nextest 0.9.151,
`--no-shuffle`, one job), on a machine shared with other workers.

`src/formats/tests.rs` (24 mutants: 20 caught, 2 missed, 2 unviable), the two
settings interleaved so they share the load:

| run | nextest, fail-fast | nextest, `--no-fail-fast` |
|---|---|---|
| 1 (load ~20) | 60s | 43s |
| 2 (load ~30) | 66s | 62s |

`src/formats/{coverage,tests,mutants}.rs` (125 mutants: 110 caught, 13
missed, 2 unviable), one run each, back to back while the load fell from ~79
to ~22: `cargo test` (the old preset) 1098s, nextest fail-fast 682s, nextest
`--no-fail-fast` 264s. The load, not the setting, made that difference: the
missed mutants, which run every test under any setting, took 3.1s, 1.5s and
1.0s each across the three runs. Within each run a caught mutant's test phase
took about as long as a missed one's (3.2s vs 3.1s, 1.8s vs 1.5s, 0.9s vs
1.0s).

So on slimfit, `--no-fail-fast` costs nothing measurable: its tests are one
binary that runs in about a second in parallel, and libtest runs a whole
binary even under `cargo test`'s fail-fast. The bound in general: a caught
mutant costs at most what a missed one already costs, one run of the whole
suite. A suite whose tests are many slow binaries (factorium: ~30s per
mutant, ADR 0001) pays up to that for each caught mutant instead of stopping
at the first failure; ADR 0002's pieces keep each command within its budget
either way.

## Consequences

- Rust gets redundant-test tasks from `next --all-mutants`, like TypeScript
  and Kotlin (and, since ADR 0005, among the mutants of a change from
  `next --mutants`).
- The Rust preset now needs cargo-nextest for mutants too; it already needed
  it for tests.
- A killed mutant costs a full test run instead of a run up to the first
  failure; see Cost.
