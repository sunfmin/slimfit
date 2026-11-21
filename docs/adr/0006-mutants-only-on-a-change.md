# 6. Mutants only on a change; redundant tests from per-test coverage

## Context

A run over every mutant cannot finish on a codebase of any size. On
factorium a `slimfit check --mutants` that fell back to a full run had 1960
mutants, each running all 213 tests: hours of work, killed (factorium#93);
building a whole-codebase mutant count in pieces measured 4 of 64 files in
7 hours (slimfit#26). The owner decided (control session, 2026-11-18 and
2026-11-19): slimfit loses the ability to run all mutants, keeps only what
finishes fast, and finds redundant tests another way, with no mutants.

## Decision

### No run mutates the whole codebase

- The only mutants run are a change's: the mutants in the lines it adds,
  and of those only the ones the agent picks (ADR 0008). Their number
  follows the change, never the codebase.
- **Lines, not files.** A suite's tool is given the change's diff
  (cargo-mutants `--in-diff`); the mutants of a whole file the change
  touches are not the change's cost.
- **No mutant count is kept.** The baseline holds no `mutants` total, per
  file or otherwise: a count of the whole codebase is what needed full
  runs. A picked mutant that survives is the change's own and fails
  `survived_mutants` (ADR 0008); surviving-mutant tasks come only from
  runs of a change.
- `--all-mutants` is refused on every command, and the fix names the line
  to use: `slimfit check --mutants`.
- A suite whose mutation tool cannot list a change's mutants and run one by
  name is refused by `--mutants` (ADR 0008); there is no other way to run
  it. Today that is every preset but Rust's.
- slimfit's own `factorium.toml` has no `[idle]` full run. `E2E_MUTANTS=1`
  runs the Rust sample's mutants of an injected change only.
- `slimfit doctor` finds `whole_codebase_mutants`: `--all-mutants`, or a
  mutation tool run without slimfit and without a diff, anywhere in
  `factorium.toml` (gates and `[idle]` steps) or the scripts it names.

A change's mutants take minutes on factorium, not hours (389s and 511s for
`check --mutants` on two real changes; evidence: slimfit#40, comment
"Evidence of slimfit#31").

### No kill matrix

Nothing reads which tests killed a mutant, so:

- the Rust preset runs nextest fail-fast: a caught mutant's test run stops
  at its first failure;
- the readers keep only each mutant's status (no `killed_by`, no list of
  known tests, no nextest log parser, no Stryker `killedBy` or PIT
  `killingTests`).

### Redundant tests: coverage proposes, the agent decides on assertions

Coverage alone is too coarse: on factorium (a406723) 86 of 207 tests reach
only functions one other test reaches, and many are not redundant
(`accounts::doctor_says_when_mytokens_lists_no_account` alone checks the
missing-mytokens branch). The literature agrees (coverage-based reduction
can lose much fault detection: Rothermel et al. ~48% in one study; Chen et
al., ICST 2017: assertions decide), and a model judging redundancy from
test text alone over-flags (~30% false positives, arXiv 2412.03693).
Together they fit: coverage proposes, the review decides.

1. **Candidate** (slimfit, deterministic, from per-test coverage of source
   lines, `reach.lines` in the measurement, ADR 0007):
   - a test whose covered lines are a subset of one other test's; of two
     tests with the same lines, the slower is proposed (on equal time, the
     later by name);
   - a test whose lines overlap another's heavily without either being a
     subset: `|A ∩ B| / |A ∪ B| ≥ 0.8` (`MERGE_OVERLAP`, a Jaccard share of
     the lines either runs); the slower of the two is proposed, so a pair
     is named once.

   The overlap is computed without the shared setup: a line more
   than half the tests run (`COMMON_LINES`, 0.5) is left out of both
   tests' lines first, and a test with no line left overlaps nothing. On
   factorium nearly every scenario runs the daemon's tick and its world's
   setup, so without this cut two scenarios that check unrelated things
   shared 80-96% of their lines; 0.5 sits in the gap between the setup
   (run by 60-80% of the tests) and the next band (families of
   scenarios), so the count barely moves with it. With the cut, 71 of 178
   candidates drop, all overlap, all sharing only setup in a sample of ten
   (evidence: slimfit#40, comment "Evidence of slimfit#34").

   The setup is each test binary's, not the suite's: a line is left out of
   a test's lines when more than half the tests of its own binary run it
   (for a language whose tests have no binary, the unit `ReachTest::binary`
   holds: its package or test file). On guild each binary has its own
   setup (a scheduler's world, a started guildd) and none reaches half the
   suite, so the suite-wide cut kept it, and 889 of 1,044 candidates paired
   two tests of one binary; cut per binary, the overlap candidates fell
   from 652 to 246 and the 392 subsets did not move (evidence: slimfit#85).

   The subset is of all lines: setup both run cannot make one test a
   subset of another, and leaving it out would drop real subsets in small
   suites, where the lines most tests run are the code under test. Each
   candidate names the one other test it most overlaps (a superset first),
   slowest candidate first. A test that runs no source line is not
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
3. **Accept**: plain `slimfit check`: failures 0; its answer shows no
   new uncovered span and test time not up (and for a merge, down), as
   trends the agent reads, not bounds (ADR 0009). No mutant runs.

**Per-test lines, for Rust.** The Rust preset's test run gives every test
its own coverage profiles (ADR 0007). After the run each test's profiles
are merged (`llvm-profdata merge`) and exported as LCOV (`llvm-cov export
-format=lcov`, with the test's binary and the build's binaries as objects,
so a binary the test starts counts), as many tests at once as there are
cores; only the records that ran are kept. That one record gives the
functions each test ran (`reach.functions`, ADR 0007) and its source
lines, test code left out (`reach.lines`), in the same coverage run: no
second test run, about 1.5s over the plain run on factorium (evidence:
slimfit#40, comment "Evidence of slimfit#32"). Other presets record
nothing per test: `next` says so in `notes`, and their suites get no
redundant-test task.

**Keep** is recorded with `slimfit keep <test> --reason "<why>"`, which the
task's steps name. `.slimfit/baseline.json`'s `kept` holds the suite, the
test, the reason and an FNV-1a hash of the test's text, from its `fn` line
to the brace that closes it, however it is shaped. The test is found by its function's
name in the suite's Rust files (of several, the one whose path names its
modules). The coverage report cannot say where, because cargo llvm-cov
leaves integration tests out of it. A candidate whose kept hash matches its
text now is not proposed; any edit inside it brings it back. `ratchet`
drops a verdict whose test changed or is gone. A test that is not found
cannot be kept: `keep` refuses, and the steps leave delete or merge.

## Consequences

- No command can run for hours on a codebase's size. A large change still
  costs its own picked mutants.
- What a deleted test killed in unchanged code is seen by nothing: a
  deletion adds no line. Redundant-test deletions rest on the assertion
  mapping, not on mutants. A test changed outside source files is likewise
  not seen by the mutants of unchanged code.
- Go, Swift, Kotlin and TypeScript suites run no mutants until their tool
  can be given a diff and a mutant's name. Their readers stay, for that.
- Old files still load: serde ignores removed fields (`killed_by`,
  `mutation_tests`, `mutated.all`, the baseline's `mutants`,
  `mutant_files`, `mutants_pending`, `mutants_at`), and the next write
  drops them. No compatibility code is kept.
