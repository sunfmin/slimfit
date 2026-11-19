# 9. Hard bounds only for what is cheap and objective; judgment is an agent's, and slimfit checks it was made, not that it is right

Each change to slimfit is weighed against this decision. The owner's words
are in slimfit#44's owner comment, and, for who picks mutants, on slimfit#58
(both in factorium's tracker).

## Context

slimfit's users are AI agents, and the tests they keep are AI-written. Some
of what slimfit asks is a number one measurement gives; the rest is
judgment: whether a test is redundant, which mutants are worth running,
what to deepen. Certainty about a judgment is not available, and checking
each verdict against ground truth costs more than the loop can pay. A rule
that is only text, on the other hand, is not followed at all (ADR 0008's
context). And a judgment made by the agent whose work it judges leans
toward that work: an author's picks of its own change's mutants all died
(ADR 0008's context).

## Decision

1. **Hard bounds only for what is cheap and objective.** A bound fails
   `check` only when its metric is a number one measurement gives without
   judgment: failing tests (`failures`), `uncovered_lines`,
   `test_seconds` (as CPU time, slimfit#43). A metric that needs judgment
   to read, or a run too costly for every `check`, is no hard bound.
   `unrecorded_mutants` is not a bound on a number but the check of point 3.
2. **Judgment is an agent's, and its uncertainty is accepted.** Which test
   is redundant and whether to delete, merge or keep it (ADR 0006); what to
   deepen: the agent in the loop decides. Which mutants are worth running:
   a second agent decides, given only the diff, the candidates and the goal
   (ADR 0008), since the author would pick what its tests already hold.
   slimfit asks that the step moves the codebase the right way most of the
   time, not every time. No routine run checks a verdict against ground
   truth: no kill matrix, no run of every mutant, no review of a verdict
   once made.
3. **slimfit checks that the step was taken, not that it was right.** The
   check is cheap and mechanical: a record the change carries (a
   `.slimfit/mutants/` record that names a picker other than its author, a
   `kept` verdict with its reason, a loosening with its reason), or a task
   `next` gives first. Who decided is the caller's word, compared as given.
   slimfit never re-decides the verdict a record holds.
4. **Drift is watched as a trend, not per verdict.** `slimfit report` shows
   what the judgments add up to (for example records without picks, the
   picks' survival by who picked beside an unbiased sample's, tests
   deleted, test time, uncovered lines), so a person can see whether the
   codebase is heading the right way.

The help's opening says it to the agent in one sentence: "Your judgment
calls are yours; slimfit checks that you made them, not that they were
right."

## Consequences

- A proposed bound on a judgment (a redundant test not deleted, a mutant
  not picked) is refused by this ADR; what can be held is that the
  judgment was recorded, and, where the author's own judgment leans, that
  another agent made it.
- A proposed check of verdicts (a sample re-run, a review of picks) belongs
  outside the loop, if anywhere: in what `report` shows over time (ADR
  0011).
- Some wrong verdicts land. They are paid for by the trend `report` shows,
  not by every landing.
