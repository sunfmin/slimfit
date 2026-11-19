# 8. The gate holds a landing's lines to a mutants run

Amends `0005-mutants-of-the-change.md` (where the change is taken from, and
`mutants_at`) and `0006-mutants-only-on-a-change.md` (who runs mutants).

## Context

ADR 0005 and 0006 made the agent that changes source run `check --mutants`
on its own change before it ratchets. That rule was only text, and nobody
followed it: in slimfit, `mutants_at` stayed at d142204 (2026-11-18 23:22)
while 88 commits (+4023/-1379 source lines, slimfit#29, #32, #33 among them)
landed without a mutants run; factorium's baseline had no `mutants_at` at all.
The next `--mutants` would have mutated thousands of lines: the whole-codebase
run back again (slimfit#36).

The gate cannot hold a single committed `mutants_at`: workers land in
parallel and merge proving into their branches, so one shared field
conflicts on every merge, or resolves to one worker's tree and makes every
other worker's measured lines unmeasured again. An independent consult
rejected that design before the issue was filed.

## Decision

Invariant: **no source line reaches the target without a passing mutants
measurement of it.** Mutants still never run in a gate (ADR 0006); the gate
only checks that a measurement covers the change.

- **The change is the landing's own diff**: what HEAD adds since its merge
  base with the target, the branch it lands on, untracked files included.
  The target is `[check] target` in `slimfit.toml`, or `origin/HEAD` (the
  remote's default branch) when unset. Not the branch's upstream: a branch
  pushed with `-u` has itself as upstream, and its pushed lines would leave
  the change unmeasured. A target set but not found is refused; with none set
  and no `origin/HEAD`, `check` holds no line (its `landing` is `null`),
  `--mutants` refuses, and `slimfit doctor` finds `no_target`.
- **Lines that must be measured**: the change's added lines in a suite's
  source files whose mutation tool can be limited to a change (no other
  suite runs mutants), neither blank nor only a `//` comment, outside
  inline tests (`#[cfg(test)]`).
- **The record of what was measured is one file per run.** A `ratchet` of a
  measurement that ran mutants writes `.slimfit/mutants/<hash>.json`: per
  file, the FNV-1a 64 hashes of the text of every line the run's diff added
  (its files' added lines, as cargo-mutants was given them). The name is the
  hash of the contents, so two branches that measured different changes
  never write the same path: merging them merges two new files, without a
  conflict, and both changes stay measured. Only such a ratchet writes one,
  and a ratchet refuses while survivors are not killed or loosened with a
  reason.
- **Measured is by text.** A required line is measured when some record holds
  its text's hash for its file. A line edited after it was measured has new
  text: unmeasured until measured again. A line moved within its file keeps
  its measurement; one moved to another file loses it.
- **Every `check` holds it**, plain ones included: per suite, a violation
  `unmeasured_mutants` with the count of required lines no record (nor the
  measurement itself, when it ran mutants) holds, and the first one's
  place. Its `fix` is the rule: `factorium worker exclusive --background --
  slimfit check --mutants` (elsewhere `nice -n 19`), then `slimfit ratchet`,
  then commit `.slimfit/mutants/` with the change. `--loosen
  unmeasured_mutants` is refused: an equivalent survivor keeps its own way
  out (`--loosen survived_mutants --reason`), whose run is recorded like any
  other, so nobody needs to fake a measurement.
- **`--mutants` runs what is left**: the landing's diff in the files that
  hold a required line no record holds. A file whose lines are all measured
  (another worker's, merged in from proving) is not run again; in a file
  that also holds an unmeasured line, the measured lines run again.
- **`mutants_at` is gone**, with the `mutants_unscoped` finding and the
  fallbacks to a pruned tree's commit and to the upstream. Old baselines
  still load (serde ignores the field) and the next ratchet drops it. A
  cached measurement keyed by it fails to load and is measured again.
- **Starting point**: code already on the target is not in any landing's
  diff, so no backlog is charged. slimfit's own baseline loses `mutants_at`
  in this change, so it starts at the tip (b6b83c0): the 4023 unmeasured
  lines before it are not charged to the next landing. This change's own
  lines were measured and recorded like any other's (Measured, below).

## Consequences

- A landing that skipped its mutants run fails the gate, at the cost of a
  plain `check`: a `git merge-base`, one `write-tree` through a copy of the
  index, one `git diff` and the records read.
- Records are never pruned: once a change reaches the target its lines are
  out of every landing's diff, but a record cannot tell whether every
  branch that carries it has landed. Each is a few hashes per line measured.
- Two lines of one file with the same text are one line to the records: a
  line added with the text of a measured one in that file counts as
  measured. A line with no mutants (`}`) costs nothing either way.
- A comment-only change needs no run; a change to a doc comment (`///`)
  does not either, as it starts with `//`.
- In a proving branch that collects several workers' landings, the change
  against `origin/HEAD` holds all of them, each covered by its own record.
- Without `origin/HEAD` (a repository whose remote was added by hand) the
  gate holds nothing until `[check] target` is set; `doctor` says so.

## Measured

This change on slimfit itself (against b6b83c0), cargo-mutants under
`factorium worker exclusive --background -- slimfit check --mutants`,
2026-11-19, a shared 18-core host at a 1-minute load of 5-18:

| run | lines to measure | files run | mutants | outcome | wall |
|---|---|---|---|---|---|
| first | 321 | 8 | 97 | 79 caught, 15 unviable, 3 missed | 1470s |
| after killing the 3 | 318 | 8 | 99 | 84 caught, 15 unviable, 0 missed | 1245s |
| after a 6-line edit of `main.rs` | 6 | 1 | 8 | 6 caught, 2 unviable | 144s |

The third run is the rule at work: the 6 edited lines were unmeasured
(plain `check` exited 1 with `unmeasured_mutants` 6), and only the one file
holding them ran again. The two records are 14.9 KB and 3.4 KB.

A plain `check` of the tree (cached test run) with the landing held: the
landing adds 324 source lines that must be measured, all recorded.

The first run's survivors: the FNV-1a hash's `^=` (no test pinned its
value; one does now), `added_lines`' count of a hunk's old side (dead: it
is gone), and the file-flag half of the "can its tool be limited" test in
`doctor` (now one `Suite::mutates`, covered where `--mutants` refuses).
