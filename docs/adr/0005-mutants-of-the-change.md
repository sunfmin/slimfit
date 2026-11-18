# 5. Mutants are measured on the change; no mutant count is kept

Supersedes `0001-mutate-only-changed-files.md` and
`0002-mutants-baseline-in-pieces.md`.

**Amended by ADR 0006** (`0006-mutants-only-on-a-change.md`): `next
--all-mutants` is gone too, so nothing runs every mutant, and redundant-test
tasks no longer come from mutants.

## Context

ADR 0001 held `survived_mutants` against a count of the whole suite's
mutants, kept per file in the baseline, and re-mutated only the files
changed since. ADR 0002 built that count in pieces, because the first one
needed every mutant measured. On factorium (1752 mutants, ~30s of tests
each) building it with `ratchet --pending-seconds 600` measured 4 of 64
files in 7 hours (16:23–23:06, load 40–60). Until it was built, no
surviving-mutant or redundant-test task could be given. The owner's
decision (2026-11-18): if the baseline has to be this slow, change it to
another way.

## Decision

- **The bound is about the change, not a count.** `check --mutants` holds
  the mutants in the lines the change added at 0 survivors: any survivor
  there is the change's own. Survivors elsewhere are tasks, not
  violations. The baseline keeps no mutant count (`mutants`,
  `mutant_files` and `mutants_pending` are gone).
- **The change** is the diff from the tree the baseline's `mutants_at`
  names to the working tree, untracked files included. `mutants_at` holds
  HEAD and the working tree written as a git tree object (through a copy
  of the index, so the real one is untouched). `init` sets it, so adopting
  slimfit runs no mutant. A ratchet with mutants measured moves it to the
  tree measured: the usual "check --mutants, ratchet, commit" leaves no
  change behind, even though HEAD moves when the commit is made.
- **Where the tree is gone** (another clone, or `git gc` pruned it: it is
  unreachable), the change is taken from `mutants_at`'s commit, which
  includes the last change measured again. Where the commit is gone too,
  or no `mutants_at` is recorded, it is taken from the merge base with the
  branch's upstream; without an upstream `--mutants` refuses, saying why
  (factorium#93: a baseline with no `mutants_at` made a plain `--mutants`
  run 1960 mutants for hours). Each is said on stderr. A `ratchet` of a
  baseline with no `mutants_at` records one, as `init` does.
- **As narrow as the tool allows.** A suite's `mutants_diff_flag` is given
  the change's diff (Rust: cargo-mutants `--in-diff`); every mutant the
  tool then reports is the change's. Without one, `mutants_file_flag`
  limits the run to the files the change touches, and a mutant is the
  change's when its line is one the diff adds. Without either (every preset
  but Rust today), `--mutants` refuses before anything runs: no path of
  `--mutants` runs every mutant; only `next --all-mutants` does.
- **Tasks come from the runs that happened.** `next --mutants` gives
  surviving-mutant tasks for the mutants that ran, and redundant-test
  tasks among them: a test that killed none of them is not named, since
  it may kill others. `next --all-mutants` runs every mutant, for the
  whole kill matrix. Nothing has to be built first.
- A survivor that changes nothing observable is accepted with
  `ratchet --loosen survived_mutants --reason`, recorded from 0 to the
  count; `mutants_at` then moves past it.
- `check --all-mutants`, `ratchet --all-mutants`, `init --mutants`,
  `--pending-seconds` and `--pending-files` are removed: with no count to
  build, nothing needs them.
- Old baselines load unchanged: serde ignores the removed fields, and the
  next ratchet writes the baseline without them. A `mutants_at` without a
  tree is taken from its commit.

## Why lines, not files

ADR 0001 rejected `--in-diff` because a per-file count can be replaced
only by a count of the whole file. With no count kept, that reason is
gone, and a change's mutants are what it costs: on factorium, mutants of
the lines a change touched, not of every file it touched.

## Consequences

- Adoption and every ratchet without `--mutants` run no mutant. A change
  costs about its own mutants (measured below).
- A change that only deletes adds no line, so no mutant runs for it. A
  deleted test can let mutants of unchanged code survive, and nothing
  holds that: before, the whole-suite count did. A redundant-test task's
  accept command is `slimfit check`, and its steps say the evidence covers
  only the mutants that ran.
- A test changed outside source files is likewise not seen by the
  mutants of unchanged code.
- A tool limited by lines may run mutants whose span reaches a changed
  line from an unchanged one (cargo-mutants does); they count as the
  change's. Told apart by line, a mutant counts by the line it starts on.
- Tree objects written for `mutants_at` are unreachable and pruned by
  `git gc` after its expiry (two weeks by default); the fallback to the
  commit only re-measures the last change.

## Measured

factorium 5cedfb4 against its parent (a real change: 49 source lines added,
6 removed, in 4 files), cargo-mutants 27.1.0 with `--jobs 4`, 2026-11-18,
1-minute load 27 on a shared host:

- the change has 26 mutants (`--in-diff`); the whole workspace has 1752;
- cargo-mutants' unmutated baseline: 130s build + 121s test;
- stopped at 300s (the owner's 5-minute cap) with 4 mutants in flight and
  none finished, so the full wall time was not measured. Each mutant costs
  an incremental build plus a test run of up to ~121s; at 4 jobs, the 26
  take about 7 rounds after the baseline.
