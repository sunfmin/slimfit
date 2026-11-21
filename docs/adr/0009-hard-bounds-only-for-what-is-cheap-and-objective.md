# 9. Hard bounds only for what is cheap and objective; judgment is an agent's, and slimfit checks it was made, not that it is right

Each change to slimfit is weighed against this decision. The owner's words
are in slimfit#44's owner comment, for who picks mutants on slimfit#58, and
for coverage on slimfit#67 (all in factorium's tracker).

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
   judgment, the same tree gives the same number on the host it runs on,
   and a gain in it cannot be had without the gain it stands for: failing
   tests (`failures`), and a picked mutant that survives
   (`survived_mutants`). A metric that needs judgment to read, a run too
   costly for every `check`, a number the host's load moves, or one an
   agent can raise without the quality it stands for, is no hard bound.
   `unrecorded_mutants` is not a bound on a number but the check of point
   3; nor is `unrecorded_growth` (ADR 0012), which a change's net source
   lines past an allowance ask for, and which a reviewed record clears
   whatever the number. These four are all `check` fails on. Line counts
   themselves are no hard bound: denser code games them.

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

   `uncovered_lines` is no hard bound either (slimfit#67). It is cheap and
   the same tree gives the same number, give or take a flap (ADR 0001),
   but an agent can lower it without the strength it stands for: a test
   that runs lines and asserts nothing counts as a gain, and only a
   picked mutant (ADR 0008) tells it from a real one. The owner's words:
   a hard bound is not friendly to an AI, which drills into it or cheats.
   So `uncovered_lines` is a trend and a task source too: `check` answers
   it against the baseline, and when it rose names the uncovered spans
   the change adds (`new_spans`), and never fails on it; `ratchet` keeps
   the lower count, `coverage_slack` its noise band; `report` shows it over
   time; `uncovered` tasks list the spans, and their steps say a fix
   asserts what the lines decide, or deletes them. Improvement comes from
   those tasks (point 2), from the feedback at the change, and from the
   trend read outside the loop (point 4).
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
   `.slimfit/growth/` record that names a reviewer other than its author, a
   `kept` verdict with its reason, a loosening with its reason), or a task
   `next` gives first. Who decided is the caller's word, compared as given.
   slimfit never re-decides the verdict a record holds.
4. **Drift is watched as a trend, not per verdict.** `slimfit report` shows
   what the judgments add up to, and what is measured but not bounded (for
   example records without picks, the picks' survival by who picked beside
   an unbiased sample's, tests deleted, test time, uncovered lines), so a
   person can see whether the
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
