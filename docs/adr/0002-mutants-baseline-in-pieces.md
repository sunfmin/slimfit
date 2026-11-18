# 2. A mutants baseline is built in pieces, each one bounded command

**Superseded by ADR 0005** (`0005-mutants-of-the-change.md`): no mutant count is kept, and `--mutants` runs the mutants of the changed lines. Its full runs (`--all-mutants`, `init --mutants`) are gone too (ADR 0006).

## Context

ADR 0001 lets `--mutants` mutate only the files changed since `mutants_at`,
but the first baseline still needs every file measured: `--all-mutants` or
`init --mutants`, one command. On factorium at 49750c6 (1752 mutants, ~30s of
tests per mutant) that is many hours, far past `hold_bound` (1800s), so a
large workspace could never get a mutants baseline through factorium. Its own
`mutants.sh` shards by file for this reason; factorium moves to slimfit.

## Decision

- Each suite in the baseline may list `mutants_pending`: source files with
  no mutant count yet. Their mutants are in neither `mutant_files` nor
  `mutants`, so `check --mutants` does not hold them, and its verdict lists
  them under `mutants_pending`.
- `slimfit ratchet --pending-seconds <s>` (and/or `--pending-files <n>`) is
  one piece. It runs as `--mutants` does (mutating the counted files changed
  since `mutants_at`), then mutates pending files in path order, one tool run
  per file, starting no file once the command's elapsed time plus the longest
  file so far would pass `s`, and stopping after `n`. Every piece measures at
  least one pending file, so repeating it always finishes. It then ratchets;
  the newly counted files are recorded, not held, since they had no count.
- A suite whose baseline has no mutants yet starts with all its source files
  pending. `init` without `--mutants`, then repeated pieces, builds the
  baseline of a large workspace.
- **Why `mutants_at` may move after a piece:** every piece re-mutates the
  counted files changed since `mutants_at` before it fills anything. So when
  it ratchets, every counted file's count is of the tree just measured, which
  is the condition ADR 0001's move of `mutants_at` rests on. Pending files
  have no count to go stale. No per-file record of when each file was
  measured is needed.
- A file new since `mutants_at` is a changed file, not a pending one: it is
  mutated and held by `--mutants` like any change. Only files that were there
  when the suite started its pieces are pending. A pending file that is
  deleted leaves the list.
- A piece is not refused because test_seconds went over its bound. Pieces
  run while other workers load the host (708s and 996s measured against a
  bound of 540s at load 45-60), and refusing threw away hours of mutant
  counts. The piece keeps the old test_seconds (it never loosens a bound)
  and lists the overrun under `not_recorded`. Failing tests, new uncovered
  lines and more surviving mutants still refuse it.
- Mutating one file per tool run costs the tool's own start (cargo-mutants
  builds and runs the unmutated tests once per run). That buys a time budget
  checked between files; one batched run could not stop at the budget.
- Before each run of the mutation tool, the reports its suite's
  `mutants_report` names are deleted, so a run that writes no report (a file
  with no mutants) is not read as the previous run's.

## Consequences

- A piece lasts about the budget plus one file; a single file whose mutants
  take longer than `hold_bound` cannot be measured this way.
- Until no file is pending, survivors in pending files are not held, and
  `next` gives no surviving-mutant tasks for them.
- A suite whose tool has no file flag cannot be built in pieces: it falls back
  to a full run, as ADR 0001's `--mutants` does.
