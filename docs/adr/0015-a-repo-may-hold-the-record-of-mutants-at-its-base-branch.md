# 15. A repo may hold the record of mutants at its base branch

## Context

ADR 0008 holds each change to a record of its mutants before it lands, so a
landing waits for its picks as well as its fast gate. In factorium the
owner wants a landing to wait for its fast gate only, and the mutants of
everything that landed on proving picked once, at promotion, by a worker
that wrote none of it, which can then pick more. Felix, 2026-11-26, in
factorium's tracker: 「我还是想把快测试结束后就合并到 proving 上面，然后在
proving 上面 promotion 的时候再去用 worker 做 mutant，统一对所有提交的
Diff 做一次可以让 worker 多挑一些」.

## Decision

- **Opt-in per repo.** `mutants_target` under `[check]` names the branch
  the record is held against (in factorium, `origin/main`, the branch
  promotion fast-forwards to). Unset, ADR 0008 holds as written.
- **Set, only a change taken against it owes a record**: a `check` whose
  target is that branch, by `--target` on `check` (plain, the listing, the
  picks) and on `ratchet`. The change is then the range HEAD adds since
  their merge base, every landing in it at once, held to one record. A
  landing's plain `check` holds none, and `slimfit next` asks it for no
  `pick-mutants`; its answer's `landing.mutants_held_at` names the branch.
- **Growth stays per landing** (ADR 0012): a range's growth is not held
  again, as each landing in it held its own.
- **The cap is a setting**, `picks_max` under `[check]`, five by default,
  so a range may be given more; the picker spreads them over its files and
  functions.
- `--loosen survived_mutants --reason` and `--no-mutants` mean the same of
  the range.

## Consequences

- A landing lands on proving before any mutant of it is picked; a gap the
  picks find is fixed at promotion, by the promotion's worker.
- `slimfit check --target origin/main` answers whether a promotion still
  owes a record: it fails with `unrecorded_mutants` until one is
  committed onto proving.
- The match is by name: the target a `check` takes its change against is
  compared with `mutants_target` as written.
