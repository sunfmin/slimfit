# 6. Mutants only on a change; redundant tests from per-test coverage

Supersedes `0004-rust-kill-matrix.md`, and what `0001`, `0002` and `0005`
still said about runs over every mutant (`--all-mutants`, `init --mutants`,
pieces, the kill matrix).

## Context

On factorium#93 (2026-11-19 02:13) a worker's `slimfit check --mutants`
fell back to a full run: 1960 mutants, each running all 213 tests with
`--no-fail-fast`, hours of work, killed. ADR 0005 already made `--mutants`
refuse rather than fall back, but `next --all-mutants` still ran every
mutant, and slimfit's own `[idle]` ran it daily (an hour or more). A full
run of factorium is many hours on this host (ADR 0002's pieces measured 4 of
64 files in 7 hours).

The owner's decision (control session, 2026-11-19 02:20): slimfit loses the
ability to run all mutants; only what finishes fast stays. And (02:35,
02:40): redundant tests are found another way, with no mutants.

## Decision

### No run mutates the whole codebase

- The only mutants run is the change's (ADR 0005): the lines changed since
  the baseline's `mutants_at`, or the merge base with the upstream. Its size
  follows the change, never the codebase.
- `--all-mutants` is gone from every command. A command line that has it is
  refused, and the fix names the line to use: `slimfit check --mutants`
  (`slimfit next --mutants` for its tasks). This refusal is the one trace
  left of the flag.
- A suite whose mutation tool takes neither a diff nor files is refused by
  `--mutants`, as before; there is no longer any other way to run it. Today
  that is every preset but Rust's (gremlins, muter, PIT, Stryker): their
  mutants run only once a suite sets `mutants_diff_flag` or
  `mutants_file_flag`.
- Surviving-mutant tasks come only from runs of a change.
- slimfit's own `factorium.toml` has no `[idle]` any more: its one step was
  the daily full run. `E2E_MUTANTS=1` runs the Rust sample's mutants of an
  injected change only (it ran every mutant of every sample).
- `slimfit doctor` finds `whole_codebase_mutants`: `--all-mutants`, or a
  mutation tool run without slimfit and without a diff, anywhere in
  `factorium.toml` (gates and `[idle]` steps) or the scripts it names.

### No kill matrix

ADR 0004 made cargo-mutants run nextest with `--no-fail-fast` and read each
mutant's log for every killing test, only so `redundant-test` could be
computed from a kill matrix. Nothing reads killers now, so:

- the Rust preset drops `--cargo-test-arg=--no-fail-fast`: a caught mutant's
  test run stops at the first failure (on factorium a caught mutant ran all
  213 tests);
- the readers keep only each mutant's status (`killed_by`, the list of known
  tests, the nextest log parser, Stryker's `killedBy`, PIT's
  `killingTests` are gone), and the samples' cargo-mutants logs with them.

### Redundant tests: coverage proposes, the agent decides on assertions

Coverage alone is too coarse. On factorium (a406723) by function coverage
86 of 207 tests reach only functions one other test reaches, and many are
not redundant: `accounts::doctor_says_when_mytokens_lists_no_account`
reaches only what `doctor_says_why_each_account_cannot_be_used_here`
reaches, yet alone checks the missing-mytokens branch. The literature
agrees (coverage-based reduction can lose much fault detection: Rothermel
et al. ~48% in one study; Chen et al., ICST 2017: assertions decide). A
model judging redundancy from test text alone over-flags (~30% false
positives, arXiv 2412.03693). Together they fit: coverage proposes, the
review decides.

1. **Candidate** (slimfit, deterministic, from per-test coverage of source
   lines, `reach.lines` in the measurement; `test_lines` before ADR 0007):
   - a test whose covered lines are a subset of one other test's; of two
     tests with the same lines, the slower is proposed (on equal time, the
     later by name);
   - a test whose lines overlap another's heavily without either being a
     subset: `|A ∩ B| / |A ∪ B| ≥ 0.8` (`MERGE_OVERLAP`, a Jaccard share of
     the lines either runs); the slower of the two is proposed, so a pair
     is named once.

   Each candidate names the one other test it most overlaps (a superset
   first), slowest candidate first. A test that runs no source line is not
   proposed.
2. **Decision** (the agent, by the task's steps). For every assertion of the
   candidate, find the assertion in the other test (or another named test)
   that checks the same outcome on the same setup. Then one of:
   - **delete**: every assertion maps; the mapping goes in the commit
     message;
   - **merge**: some assertion does not map, but the setups are compatible:
     fold the unmapped assertions (and the setup they need) into the other
     test, or turn both into one test over a table of cases, so the suite
     pays the setup once (a factorium scenario's cost is its world, about 50
     git processes; factorium c277f9a merged this way). The merged test
     keeps every assertion of both, each failing with its own message; the
     commit lists where each assertion went;
   - **keep**: the setups conflict (folding one changes what the other
     checks), or merging would hide which behaviour failed.
3. **Accept**: plain `slimfit check`: no new uncovered line, failures 0,
   test time not up (and for a merge, down). No mutant runs.

**Built (slimfit#32), for Rust.** The Rust preset's test run already gives
every test its own coverage profiles (ADR 0007). After the run each test's
profiles are merged (`llvm-profdata merge`) and exported as LCOV
(`llvm-cov export -format=lcov`, with the test's binary and the build's
binaries as objects, so a binary the test starts counts), as many tests at
once as there are cores; only the records that ran are kept. That one
record gives the functions each test ran (`reach.functions`, ADR 0007) and
its source lines, test code left out (`reach.lines`), in the same coverage
run: no second test run. Other presets record nothing per test: `next`
says so in `notes`, and their suites get no redundant-test task.

**Keep** is recorded with `slimfit keep <test> --reason "<why>"`, which the
task's steps name. `.slimfit/baseline.json`'s `kept` holds the suite, the
test, the reason and an FNV-1a hash of the test's text, from its `fn` line
to the brace rustfmt closes it with. The test is found by its function's
name in the suite's Rust files (of several, the one whose path names its
modules). The coverage report cannot say where, because cargo llvm-cov
leaves integration tests out of it. A candidate whose kept hash matches its
text now is not proposed; any edit inside it brings it back. `ratchet`
drops a verdict whose test changed or is gone. A test that is not found
cannot be kept: `keep` refuses, and the steps leave delete or merge.

## Consequences

- No command can run for hours on a codebase's size. A large change still
  costs its own mutants (Measured, below).
- What a deleted test killed in unchanged code is seen by nothing: a
  deletion adds no line (ADR 0005). Redundant-test deletions rest on the
  assertion mapping, not on mutants.
- Go, Swift, Kotlin and TypeScript suites run no mutants until a diff or
  file flag is set for their tool. Their readers stay, for that.
- Old files still load: serde ignores the removed fields (`killed_by`,
  `mutation_tests`, `mutated.all` in a cached measurement; the baseline's
  `mutants`, `mutant_files`, `mutants_pending` from before ADR 0005), and
  the next write drops them. No compatibility code is kept.

## Measured

`slimfit check --mutants` (this ADR's build, cargo-mutants 27.1.0, nextest
0.9.151, the Rust preset without `--no-fail-fast`) on a clone of factorium,
each commit checked out with the baseline's `mutants_at` at its parent,
2026-11-19, under factorium's `worker exclusive --background`, 1-minute load
2-19 on a shared 18-core host:

| change | lines added (all / `src` .rs) | mutants | outcome | mutants run | `check --mutants` wall |
|---|---|---|---|---|---|
| f1fc25c (factorium#110) | 69 / 32 in 4 files | 4 | 4 unviable | 3 min | 389s |
| 3b6afc1 (factorium#83) | 48 / 30 in 2 files | 16 | 2 caught, 14 unviable | 7 min | 511s |

The wall time is one instrumented test run, plus cargo-mutants' unmutated
baseline (100-118s build, 66-69s test), plus the mutants: an unviable one
about 1s, a caught one an incremental build (7s) and its test run (18s and
200s here; the second mutant made a test run long before it failed). The
whole workspace has about 1960 mutants (factorium#93).

### Per-test lines (slimfit#32)

factorium aecedfe: a clone with its build warm, 234 tests. Each run was
made under factorium's `worker exclusive`, one after the other, on
2026-11-19, with a 1-minute load of 6-14 on a shared 18-core host. Wall
time of the Rust preset's test command (build, tests, coverage report and
the per-test record):

| test command | round 1 | round 2 |
|---|---|---|
| no per-test record (`cargo llvm-cov nextest`, no runner) | 20.9s | 20.9s |
| ADR 0007 as first built: `llvm-profdata show` per profile, one at a time (functions only) | 24.9s | 25.0s |
| this ADR: `llvm-profdata merge` + `llvm-cov export` per test, one per core (functions and lines) | 22.5s | 22.4s |

So the per-test record costs about 1.5s over the plain run, less than the
functions-only record it replaces. `slimfit next --fresh` measured 23.0s
(`command_seconds`). The measurement grew to 7.7 MB (`reach.lines`), and
`reach.txt` is 16 MB. The function of each of the 234 tests was found,
for `keep`.

`slimfit next --all --kind redundant-test` counts **180** candidates (179
in an earlier run of the same tree: coverage that depends on timing). They
come from the 230 tests that run a source line: 44 subset candidates (by
function coverage, the control session found 86) and 136 by overlap alone.
factorium's scenarios all run the daemon's tick and their world's setup,
so most pairs share more than `MERGE_OVERLAP` of their lines. At 0.9 there
would be 102 overlap candidates, at 0.95 there would be 68. Each is still
decided by its assertions. Keeping one test with `slimfit keep` (0.04s,
from the cached measurement) left the count at 179, and the test was
proposed again once a line inside it changed.
