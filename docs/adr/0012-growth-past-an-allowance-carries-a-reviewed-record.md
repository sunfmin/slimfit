# 12. Growth past an allowance carries a record a second agent judged; the gate holds the record, never the number

## Context

slimfit records each suite's `prod_lines` and `test_lines` and bounded
neither. Nothing in a change's loop asked whether its growth was needed,
and repos grew without anyone deciding they should: guild reached about
113k source and 99k test lines (the owner, 2026-11-21: "guild 现在有了 22
万行代码。我觉得太多了。不知道他做了什么"), and guild#24 asked for a hard
per-crate line budget in guild's own gate. The owner wants every repo kept
from bloating, slimfit itself included (slimfit#75, in factorium's
tracker).

A hard line bound is refused by ADR 0009, point 1: lines can be gamed
(denser code), and a bound an agent can meet without the gain it stands for
drills it into cheating. Point 3 allows what is wanted instead: slimfit
checks that a step was taken, not that it was right. And as with picking
mutants (ADR 0008), the author's own judgment leans: an agent asked whether
its own code was needed says yes.

## Decision

Invariant: **no change that adds more than `[check] growth_allowance`
source lines net reaches the target without a growth record that names a
reviewer other than its author.**

- **Net source lines** of a change (the change of ADR 0008: since HEAD's
  merge base with the target, with `mutants_from` as its floor) are, per
  suite, the source lines (as `prod_lines` counts them) of the files the
  change touches now, less theirs where the change starts, summed over the
  suites. Both sides are read by today's classifier; the old side's texts
  come from one `git cat-file --batch`. Code the change deletes pays for
  what it adds. Test lines are not counted: fewer, stronger tests are the
  other tasks' business.
- **The allowance** defaults to 50. Of slimfit's first 73 landings, the
  median added 22 source lines net, and 21 (29%) added more than 50 (the
  largest 418). So a fix or a small task needs no record, and about one
  landing in three pays one more agent call, the ones whose size is worth a
  second look. A repo may set its own.
- **A second agent judges**, at the same point in the loop as the picker:
  after the change, before `ratchet`. It is given the goal (`slimfit help
  reviewer`, its one home) and the diff, nothing of the author's
  reasoning: could this change be done with fewer lines, or by deleting or
  reusing code that exists? It answers `as-is` with its why, or `cut`,
  naming what to cut. The author makes the cuts (and when the change then
  falls to the allowance, no record is due) or answers each.
- **The record.** `ratchet --serves <issue> --considered <what was weighed
  for reuse or deletion> --as-is <why> | --cut <what> --answer <what was
  done> --reviewer <id> --author <id>` writes `.slimfit/growth/<hash>.json`:
  the tree, the net lines then, and those fields. Every field is one line;
  the reviewer must differ from the author. The name is the hash of the
  contents, so branches merge without a conflict.
- **The gate.** Every `check` fails with `unrecorded_growth` (never
  loosened) while the change adds more than the allowance net and adds no
  file in `.slimfit/growth/` whose `reviewer` and `author` are both given and
  differ. That comparison is all it reads of a record: it does not judge
  the verdict, the cuts, or which lines a record covers. Never on the
  number alone: past the allowance, the number only asks for the step.
- **The loop asks first.** `slimfit next` gives a `justify-growth` task,
  right after `pick-mutants`, with the net lines and the files that grew.
- **Drift shows as a trend.** `slimfit report` counts the growth records
  by verdict (`growth_records`), now and at each history point beside its
  `prod` and `test` lines: lines that keep climbing while every record
  says `as_is` is a review kept in form only.

ADR 0009's point 1 gains `unrecorded_growth` beside `unrecorded_mutants`:
not a bound on a number, but the check of its point 3.

## Consequences

- guild#24's hard per-crate budget is not needed: guild gets this from
  slimfit's gate.
- A change past the allowance costs one more agent call, to read a diff.
- The ids are not verified, and a reviewer can wave anything through: the
  step is held, not its rightness (ADR 0009, point 3); `report` is where a
  hollow review shows.
- Splitting a change into landings under the allowance passes each
  without a record. Each split lands on its own and is seen by `report`'s
  history; a record per landing would cost more than it would catch.
- A change past the allowance that also adds source lines needs both
  records: the mutants record and the growth record answer different
  questions and are written by different second agents (or one agent asked
  both, separately).
- The gate's check adds one `git cat-file --batch` to `check`.
- Without a target, `check` holds no growth either, as with mutants
  (`doctor` finds `no_target`).
