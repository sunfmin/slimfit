# 9. Hard bounds only for what is cheap and objective; judgment is the agent's, and slimfit checks it was made, not that it is right

Each change to slimfit is weighed against this decision. The owner's words
are in slimfit#44's owner comment (factorium's tracker:
`factorium issue show slimfit 44`).

## Context

slimfit's users are AI agents, and the tests they keep are AI-written. Some
of what slimfit asks is a number one measurement gives; the rest is
judgment: whether a test is redundant, which mutants are worth running,
what to deepen. Certainty about a judgment was never available, and
checking each verdict against ground truth costs more than the loop can
pay. A rule that is only text, on the other hand, is not followed at all
(ADR 0008's context).

## Decision

1. **Hard bounds only for what is cheap and objective.** A bound fails
   `check` only when its metric is a number one measurement gives without
   judgment, and the same tree gives the same number on the host it runs
   on: failing tests (`failures`) and `uncovered_lines`. A metric that
   needs judgment to read, a run too costly for every `check`, or a number
   the host's load moves, is no hard bound. `unrecorded_mutants` is not a
   bound on a number but the check of point 3.

   `test_seconds` is no hard bound (slimfit#59). It was one, as CPU time
   (slimfit#43, ADR 0010), on the claim that load does not move CPU time.
   On the owner's host (Apple M5 Max, two perf levels) it does: slimfit#58's
   worker measured the unchanged parent fe8b1cc at 12.005 s against a
   baseline of 8.816 s (limit 11.579 s) at a 1-minute load of about 22, and
   of 7 red gates of slimfit since slimfit#43, 6 were `test_seconds`
   (11.8-13.3 s against 11.58 s), none known to be a real slowdown. The
   falls in test time came from `slow-test` tasks (sesame#15: 3.771 ->
   3.149 s), not from the bound. So `test_seconds` is a trend and a task
   source: `check` answers it against the baseline and never fails on it,
   `ratchet` keeps the lower time, `report` shows it over time, and
   `slow-test` tasks rank by it. A gate's time is bounded by the gate's
   own limit (factorium's `fast_limit` / `slow_limit`).
2. **Judgment is the agent's, and its uncertainty is accepted.** Which test
   is redundant and whether to delete, merge or keep it (ADR 0006); which
   mutants are worth running (slimfit#42); what to deepen. slimfit asks that
   the step moves the codebase the right way most of the time, not every
   time. No routine run checks a verdict against ground truth: no kill
   matrix, no run of every mutant, no second opinion.
3. **slimfit checks that the step was taken, not that it was right.** The
   check is cheap and mechanical: a record the change carries (a
   `.slimfit/mutants/` record, a `kept` verdict with its reason, a
   loosening with its reason), or a task `next` gives first. It never
   re-decides the verdict the record holds.
4. **Drift is watched as a trend, not per verdict.** `slimfit report` shows
   what the judgments add up to, and what is measured but not bounded (for
   example records without picks, tests deleted, test time, uncovered
   lines), so a person can see whether the
   codebase is heading the right way.

The help's opening says it to the agent in one sentence: "Your judgment
calls are yours; slimfit checks that you made them, not that they were
right."

## Consequences

- A proposed bound on a judgment (a redundant test not deleted, a mutant
  not picked) is refused by this ADR; what can be held is that the
  judgment was recorded.
- A proposed check of verdicts (a sample re-run, a second agent's review)
  belongs outside the loop, if anywhere: in what `report` shows over time.
- Some wrong verdicts land. They are paid for by the trend `report` shows,
  not by every landing.
