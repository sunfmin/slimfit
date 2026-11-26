# 16. The picker sizes its picks to the diff

## Context

ADR 0008 capped a change's picks at five; ADR 0015 made the cap a setting,
`[check] picks_max`, so a promotion's range could be given more. A fixed
number is wrong both ways: too many for a one-line fix, too few for a range
of many landings. Felix, 2026-11-26: 「AI 挑几个 mutant，测试的那个，不用给
他固定数字了，让 AI 自己判断好了」, then 「让他自己根据 diff 的大小来判断」.

## Decision

- **No cap and no formula.** The picker decides how many, scaled to the
  diff: how many lines it adds and how many decisions it makes, spread
  over its files and functions. `check --mutants` runs every name given.
- **The guards are per pick and in the report, not a number.** Every pick
  has its own one-line `--why`; nothing worth a mutant is still
  `--no-mutants` with a reason. Each record keeps `added_lines`, the source lines
  its change adds, and `slimfit report` lists each record's picks against
  them, so a picker that picks too few or too many shows there.
- **What stays**: no command or setting runs every mutant of a change or
  mutates the whole codebase (ADR 0006); each mutant is named one by one.
- `picks_max` is retired: a `slimfit.toml` that sets it is accepted and
  ignored for one release, and `slimfit doctor` names the line to delete.

## Consequences

- A promotion's range gets more picks without a setting to raise.
- A run's cost still follows the picks, now the picker's count.
