# 8. A second agent picks the mutants; the gate holds a record that names it

## Context

Mutants run only on a change (ADR 0006), so they must run on each change
that adds source, and a written rule alone is not followed (slimfit#36: 88
landings went without a run). Running every mutant of a change costs too
much: most of a listing cannot build (12 of 17 on factorium's #107 change,
459 of 846 in a factorium#117 run), and holding each added line to a
measured mutant turned proving red and disagreed between clones
(slimfit#37, slimfit#38). So a few mutants are picked, at most five
(slimfit#42), and the guarantee that no mutant of the change survives is
given up on purpose.

Who picks matters. The agent that wrote a change picks what its own tests
already hold: 110 picks by authors across four repositories gave 105
killed, 5 unviable and 0 survived, where runs of every mutant had found
survivors that mattered (factorium#119: 4; sesame#7's Content-Type
default; factorium#128's `in_session` mutant, which would have signalled
every process of the owner's). The owner's decision, in the owner's words,
is on slimfit#58 in factorium's tracker.

## Decision

Invariant: **no change that adds source lines reaches the target without a
record of its mutants that names a picker other than its author**: the
picks a second agent made, why, and their outcomes; or that agent's reason
none is worth one. Mutants never run in a gate (ADR 0006).

- **The change** is what HEAD adds since its merge base with the target
  (`[check] target`, or `origin/HEAD`), untracked files included, with the
  baseline's `mutants_from` as a floor while the target is behind it. Its
  source lines are those neither blank nor only a `//` comment, outside
  inline tests, in suites whose tool can list a change's mutants and run
  one by name (`mutants_diff_flag` and `mutants_pick_flag`; cargo-mutants'
  `--in-diff` and `-F`).
- **The candidates.** `check --mutants` with no name lists them
  (cargo-mutants `--list --json --in-diff`: name, file, line, function,
  replacement) without building.
- **A second agent picks**, at the same point in the loop as ever: after
  the change, before `ratchet`, before landing; not as idle work. It is
  given the goal (`slimfit help picker`, its one home), the change's diff
  and the candidates, and nothing of the author's reasoning. It picks at
  most five it thinks most likely to survive (decisions the change makes:
  comparisons, boolean logic, guards, return values that matter), skips
  those that cannot build, and gives a why for each; or says why none is
  worth one. In factorium the command that starts it is factorium's own;
  `slimfit help factorium` says how.
- **The tool runs only those.** `check --mutants <name>... --why <why>...`
  refuses a name the listing does not hold, then runs each pick by its
  anchored name (`-F '^…$'`), at background priority, each with its
  function's tests and timeout (ADR 0007). A pick that survives fails as
  `survived_mutants`: the author kills it (a fast test through the
  interface, or simpler code) before landing, or `ratchet --loosen
  survived_mutants --reason` when it changes nothing observable.
- **The record.** `ratchet --picker <id> --author <id>` after that run
  writes `.slimfit/mutants/<hash>.json`: the tree it was made of, the two
  ids, and per suite the picks with why and outcome. `ratchet --no-mutants
  "<why>" --picker <id> --author <id>` writes one with the reason and no
  picks. An id is opaque, the caller's word (a session id with harness and
  model). `ratchet` refuses a record without both ids, or with the two
  equal. The ids are given to `ratchet`, which writes the record, and not
  to `check`: a measurement is of the tree, and is cached by it. The name
  is the hash of the contents, the tree among them, so two branches never
  write the same path and merge without a conflict.
- **The gate.** Every `check`, plain ones included, fails with
  `unrecorded_mutants` (never loosened) when the change adds source lines
  and adds no file in `.slimfit/mutants/` whose `picker` and `author` are
  both given and differ, unless the measurement itself ran picks (its
  `ratchet` writes the record). That comparison is all it reads of a
  record: it does not judge a pick, ask who the ids are, or ask which lines
  a record covers. A record that does not parse names no picker. Merging
  the target in moves the merge base, so other landings' lines and records
  both drop out.
- **The loop asks first.** `slimfit next` gives a `pick-mutants` task,
  right after `failing-test`, while the change adds source lines and no
  such record, with the candidates as evidence; its steps say to ask a
  second agent with the goal topic, not to pick yourself.
- **Drift shows as a trend.** `slimfit report` counts records with picks
  against those without (`mutants_records`) and tallies the picks'
  survival by who picked (`picker`; `author` for a record that names no
  other picker) beside the unbiased sample's (ADR 0011). A survivor is
  killed before its record is written, so the record's last outcome reads
  like a pick the tests already held: since slimfit#68 each pick also keeps
  its first outcome (`first_outcome`, from the first `check --mutants` that
  ran it since the last record, kept in `.slimfit/out/` until then), and
  the tally counts first-run survivors (`first_survived`) by who picked:
  the gaps a second agent's picks find against the author's. Records from
  before it count by their picker as before, their first outcome unknown
  (`first_unknown`).

## Consequences

- A mutant nobody picked is not run; its survival is not seen. That is the
  price of the decision, paid on purpose; the sample (ADR 0011) measures
  what it misses.
- Each change that adds source costs one more agent call, to read a diff
  and a listing: no build, no test run.
- The ids are not verified: an author that names itself under two ids
  passes. slimfit checks that the step was taken, not who took it (ADR
  0009, point 3); a `picker` rate far below the `sampled` one in `report`
  is where a hollow step shows.
- The gate's check is a `git merge-base`, a `write-tree`, one `git diff`
  and a read of the records the change adds.
- A change that only deletes code, or only touches tests, adds no source
  line and needs no record.
- In a proving branch that collects several landings, one record naming a
  second picker anywhere in the change against `origin/HEAD` passes all of
  them; each landing was held on its own branch before it got there.
- Without `origin/HEAD` the gate holds nothing until `[check] target` is
  set; `doctor` says so (`no_target`).
