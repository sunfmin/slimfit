# 1. Coverage noise is absorbed by a slack, not by re-measuring or by tracking lines

## Context

`uncovered_lines` is not always a function of the code. factorium's tests spawn
its binary under llvm-cov, and which run reaches a line can depend on timing:
on factorium 90916ff with nothing changed, five runs gave 257, 256, 257, 256,
256 (slimfit#5). `check` called 256 an improvement, `ratchet` locked it in,
and the next 257 failed the gate for no code.

## Decision

`[check] coverage_slack` (lines, default 1) is a dead band around the baseline:

- `check` fails only when `uncovered_lines > baseline + coverage_slack`;
- it reports an improvement only when `uncovered_lines < baseline - coverage_slack`;
- `ratchet` keeps the baseline's `uncovered_lines` unless the run improved by
  that rule (or `--loosen uncovered_lines` is given).

The default is 1 because `slimfit init` must hold on a fresh repository with
no hand-tuned config, and 1 is the flap measured. A suite that flaps more sets
a larger value; a deterministic suite can set 0.

## Alternatives rejected

- **Measure N times and take the worst:** multiplies the cost of every run
  (the coverage run is the slow part of a gate) to fix a few lines.
- **Exclude lines seen to flap:** needs per-line history kept across runs and
  trees (the cache is per working tree, gates run in fresh worktrees), and a
  line that moves with an edit loses its history. Much more state for the
  same result.

## Consequences

Up to `coverage_slack` new uncovered lines pass `check` once, but the baseline
never rises without `--loosen`, so they cannot accumulate; `slimfit next` still
lists them as `uncovered` tasks. A real gain of `coverage_slack` lines or fewer
is not locked in by itself; it is locked in with the next gain that clears the
band.
