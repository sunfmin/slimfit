# 8. The agent picks the mutants; the gate holds that a record exists

Amends `0005-mutants-of-the-change.md` (where the change is taken from, and
`mutants_at`) and `0006-mutants-only-on-a-change.md` (who runs mutants, and
which).

## Context

ADR 0005 and 0006 made the agent that changes source run `check --mutants`
on its own change before it ratchets. As a written rule nobody followed it:
in slimfit, `mutants_at` stayed at d142204 while 88 landings went without a
mutants run (slimfit#36). The gate had to hold it.

The first gate (slimfit#36) held every source line a landing adds to a
ratcheted run of all its mutants, by records of per-line text hashes. It
held, and it cost too much:

- Running every mutant of a change is slow, and most of a listing cannot
  build (a body replaced by `Default::default()` for a type with no
  default): 12 of 17 on factorium's #107 change, 108 of 117 and 459 of 846
  in factorium#117's runs.
- Per-line coverage turned proving red twice in a day (slimfit#37,
  factorium#121), and clones disagreed about how a diff aligns, so a line
  measured in one was unmeasured in another (slimfit#38).

The owner decided (slimfit#42, in factorium's tracker, control session
2026-11-19): the agent picks at most five, and the guarantee that no
mutant in the change's lines survives is dropped on purpose — 「做五吧，那个
硬保证不要了」 ("make it five; drop the hard guarantee"). An independent
consult reviewed this design before the issue was filed and recommended it.

## Decision

Invariant: **no change that adds source lines reaches the target without a
record of its mutants**: the picks it ran, why, and their outcomes; or why
none was worth one. The guarantee is the agent's picks, not every mutant.
Mutants still never run in a gate (ADR 0006).

- **The change** is unchanged: what HEAD adds since its merge base with the
  target (`[check] target`, or `origin/HEAD`), untracked files included,
  with the baseline's `mutants_from` as a floor while the target is behind
  it. Its source lines are those neither blank nor only a `//` comment,
  outside inline tests, in suites whose tool can list a change's mutants
  and run one by name (`mutants_diff_flag` and `mutants_pick_flag`;
  cargo-mutants' `--in-diff` and `-F`). `mutants_file_flag` is gone: a tool
  limited only to files would run every mutant of them.
- **The agent picks.** `check --mutants` with no name lists the candidates
  (cargo-mutants `--list --json --in-diff`: name, file, line, function,
  replacement) without building, and that is its answer. The agent reads its
  change and picks at most five, on the decisions it makes (comparisons,
  boolean logic, match guards, a return value that matters), skipping those
  that cannot build.
- **The tool runs only those.** `check --mutants <name>... --why <why>...`
  refuses a name the listing does not hold, then runs each pick by its
  anchored name (`-F '^…$'`), at background priority, each with its
  function's tests and timeout (ADR 0007). A pick that survives fails as
  `survived_mutants`: kill it, or `ratchet --loosen survived_mutants
  --reason`.
- **The record.** `ratchet` after that run writes
  `.slimfit/mutants/<hash>.json`: the tree it was made of and, per suite,
  the picks with why and outcome. `ratchet --no-mutants "<why>"` writes one
  with that reason and no picks. The name is the hash of the contents, the
  tree among them, so two branches never write the same path and merge
  without a conflict.
- **The gate.** Every `check`, plain ones included, fails with
  `unrecorded_mutants` (never loosened) when the change adds source lines
  and adds no file in `.slimfit/mutants/`, unless the measurement itself
  ran picks (its `ratchet` writes the record). It does not read the record,
  nor ask which lines it covers. The per-line hashes, their matching and
  everything only they served are removed. Merging the target in moves
  the merge base, so other landings' lines and records both drop out.
- **The loop asks first.** `slimfit next` gives a `pick-mutants` task,
  right after `failing-test`, while the change adds source lines and no
  record, with the candidates as evidence.
- **Empty compliance shows.** The consult's stated risk is a trivial pick or
  a vague "none" on every change; `slimfit report` counts the records with
  picks against those without (`mutants_records`).

## Consequences

- A mutant nobody picked is not run; its survival is not seen. That is the
  price of the decision, paid on purpose.
- The gate's check is a `git merge-base`, a `write-tree` and one `git
  diff`: no record is read, so no record format can turn it red.
- Per-line records from before this decision (`{"lines": …}`) load as
  `older` in `report`; slimfit's own were removed in this change.
- A change that only deletes code, or only touches tests, adds no source
  line and needs no record.
- In a proving branch that collects several landings, one record anywhere
  in the change against `origin/HEAD` passes all of them; each landing was
  held on its own branch before it got there.
- Without `origin/HEAD` the gate holds nothing until `[check] target` is
  set; `doctor` says so (`no_target`).

Measurements of this decision (listing time, picked runs against full runs)
are in the comments on slimfit#42.
