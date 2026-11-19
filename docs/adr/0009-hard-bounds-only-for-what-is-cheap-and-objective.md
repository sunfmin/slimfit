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
   judgment: failing tests (`failures`), `uncovered_lines`,
   `test_seconds` (as CPU time, slimfit#43). A metric that needs judgment
   to read, or a run too costly for every `check`, is no hard bound.
   `unmeasured_mutants` is not a bound on a number but the check of point 3.
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
   what the judgments add up to (for example records without picks, tests
   deleted, test time, uncovered lines), so a person can see whether the
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
