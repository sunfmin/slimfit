# 1. `--mutants` mutates only the files changed since the baseline

## Context

Mutation testing a large workspace in full takes hours. factorium at 90916ff
has 1752 cargo-mutants mutants, and each one runs its tests for about 30s.
factorium ends any heavy command that runs past `hold_bound` (1800s), so a
gate running `slimfit check --mutants` over every mutant can never pass.

## Decision

- The baseline records `mutants_at`: the commit its mutant counts were
  measured at, plus the files that differed from that commit when they were
  measured. Each suite also gets `mutant_files`, its mutant counts per file,
  which add up to `mutants`.
- `--mutants` (on `check`, `next`, `ratchet`) mutates only the source files
  that changed since `mutants_at`: files changed since that commit, untracked
  files, and the files that were dirty then. Each one is passed to the tool
  with the suite's `mutants_file_flag` (`--file` for cargo-mutants). The
  changed files get their counts from this run, every other file keeps its
  baseline count, and `survived_mutants` is held against the sum, as before.
  Ratcheting the result moves `mutants_at` to the tree just measured.
- `--all-mutants` mutates every file. `init --mutants` does a full run. A
  `--mutants` run also falls back to a full run, saying why on stderr, when
  any of these holds:
  - the baseline has no `mutants_at`;
  - its commit is not in the repository (for example, squashed away);
  - the suite's baseline has no per-file counts that add up to its total
    (baselines older than this decision);
  - the suite's tool has no file flag (every preset except Rust).
- The output's `mutated` field says, per suite, `"all"` or which files were
  mutated.

## Why whole files, not `cargo mutants --in-diff`

`--in-diff` mutates only the changed lines, so it says nothing about the
rest of a changed file. A per-file count can be replaced only by a count of
the whole file. Merging at the level of single mutants would need every
mutant's outcome in the committed baseline, and mutants have no identity
that survives an edit: their names carry line and column numbers. So the
unit is the file. That is also what factorium's own `mutants.sh -f` does.

## Consequences

- A run costs about as much as the mutants of the touched files, not of the
  whole workspace.
- A test changed outside source files (`tests/`, fixtures) can change which
  mutants of *unchanged* files die. `--mutants` does not see that; only
  `--all-mutants` does. Tests inside source files (Rust's `#[cfg(test)]`
  blocks) re-mutate their own file. So the `redundant-test` task, whose
  whole job is deleting a test, is accepted with `slimfit check
  --all-mutants`.
- A partial run's kill matrix covers only the mutated files, so `next` gives
  no `redundant-test` tasks from it. Its `surviving-mutant` tasks name only
  the mutated files.
