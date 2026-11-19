# 11. An unbiased sample of landed changes' mutants, out of the loop

## Context

The agent that writes a change also picks its mutants (ADR 0008), so its
picks may lean toward what its tests already hold: in the first day of
picks, 110 across four repositories, 0 survived, where the full runs before
ADR 0008 found survivors. ADR 0009 says where a check of verdicts belongs:
outside the loop, in what `report` shows over time. The owner asked for
one (slimfit#55).

## Decision

- **The draw.** `slimfit next --sample <N>` draws from the mutants of the
  source lines HEAD's history added since the last committed sample (its
  `tip`), or since the first-parent commit 7 days back, never before the
  baseline's `mutants_from`. Names a record added in that span picked are
  left out, compared without their line and column, since later edits move
  them. The draw is uniform, by SplitMix64 seeded with the hash of HEAD's
  commit: the same tip draws the same, so nobody can draw again for a
  better result.
- **The run.** The drawn mutants run as picks run (ADR 0007: each with its
  function's tests). One that does not build is replaced by the next in the
  draw, for at most 3 runs, so N means N viable where the pool allows.
- **Where it runs.** On factorium's idle tasks line
  (`slimfit next --all --sample 5`), at its background priority. It is a
  flag of `next` so that the line still prints a single `next --all` answer.
- **What it answers.** Each survivor is a `surviving-mutant` task with
  `evidence.source = "sampled"` (a pick's is `"author"`). The sample itself
  becomes a `record-sample` task whose evidence carries the record's text.
  Idle work commits nothing, so the record reaches git only through that
  task's worker, who writes it to `.slimfit/samples/<tip>.json` and
  commits it. The next sample starts from that tip.
- **Never a bound.** `check` and `ratchet` read no sample, and no record's
  verdict is re-decided (ADR 0006, 0008, 0009 stand).
- **The tally.** Each `report` history point has `mutants_survival`, a
  map from the source that chose the mutants to `{ran, survived, rate,
  unviable}`, over the records and samples committed by then. Today the
  sources are `author` (picks in records) and `sampled`. Another source
  (slimfit#58's second picker) adds a key; the shape stays the same.

## Consequences

- One small issue per sample, to commit its record. Without it the tally
  would count only the samples that had survivors, which is biased.
- A sample that runs again while the previous record has not landed yet
  draws again from the same span, seeded with its own tip. Its
  `record-sample` task is not filed while one is open.
- The sample is a set of names drawn at one tip, run on the tree at that
  tip. A name whose code later changed is not drawn again.

Its cost on slimfit is in the comment "Evidence of slimfit#55" on
slimfit#55.
