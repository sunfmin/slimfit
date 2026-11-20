# 1. Coverage is a trend and a task source, not a bound; its noise is absorbed by a slack

## Context

`uncovered_lines` is not always a function of the code. factorium's tests spawn
its binary under llvm-cov, and which run reaches a line can depend on timing:
on factorium 90916ff with nothing changed, five runs gave 257, 256, 257, 256,
256 (slimfit#5). `check` called 256 an improvement, `ratchet` locked it in,
and the next 257 failed the gate for no code.

`uncovered_lines` was then a hard bound: `check` failed when it rose past the
baseline plus the slack. slimfit#67 took that away. The owner's reason: a hard
bound is not friendly to an AI author, which drills into it or games it. A
bound on uncovered lines rewards a test that runs lines without asserting
anything: it counts such a test as a gain, and only a mutant a second agent
picks (ADR 0008) tells it apart from a real one. What a hard bound holds is
what is cheap and objective (ADR 0009): failing tests, and the change's
record of its mutants.

## Decision

`uncovered_lines` is a trend and a task source, not a bound:

- `check` never fails on it. It answers each suite's `uncovered_lines`
  against the baseline (`verdict.uncovered_lines`), and when it rose, names
  the uncovered spans that hold a line the change adds since the branch it
  lands on (`new_spans`: file, lines, function). That is feedback for the
  author at the change, not a block.
- `slimfit next` lists `uncovered` tasks, as before. Their steps say what a
  fix is: a fast test through the interface that asserts what the lines
  decide, or deleting the code if nothing needs it. A test that only runs
  the lines is not a fix.
- `ratchet` still records it and only tightens it, so `report`'s history
  stays continuous. `--loosen uncovered_lines` is refused: there is no bound
  to raise.

`[check] coverage_slack` (lines, default 1) stays as the noise band of that
trend:

- `check` reports an improvement only when `uncovered_lines < baseline - coverage_slack`;
- `ratchet` keeps the baseline's `uncovered_lines` unless the run improved by
  that rule.

The default is 1 because `slimfit init` must hold on a fresh repository with
no hand-tuned config, and 1 is the flap measured. A suite that flaps more sets
a larger value; a deterministic suite can set 0.

## Alternatives rejected

- **Keep the bound:** it is what an agent games (a test that runs lines and
  asserts nothing), and what it rewards is only told apart by a mutant.
- **Measure N times and take the worst:** multiplies the cost of every run
  (the coverage run is the slow part of a gate) to fix a few lines.
- **Exclude lines seen to flap:** needs per-line history kept across runs and
  trees (the cache is per working tree, gates run in fresh worktrees), and a
  line that moves with an edit loses its history. Much more state for the
  same result.

## Consequences

New uncovered lines land when their author judges them fine; they are named
at the change (`new_spans`) and stay `uncovered` tasks until a test asserts
what they decide or they are deleted. The baseline keeps the lowest count
ratcheted, so a rise does not show in `report`'s history: it shows in the
`check` of the change that brought it. A real gain of `coverage_slack` lines
or fewer is not locked in by itself; it is locked in with the next gain that
clears the band.
