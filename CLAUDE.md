# slimfit

A CLI that keeps a codebase small and its tests few, fast and strong. It is used by AI agents: they change code, and slimfit measures the change and ratchets the result. `slimfit --help` is the agents' manual. Keep it true whenever behaviour changes.

## Layout

- `src/formats/`: readers for the report files the tools write (JUnit, go test JSON, LCOV, Go coverprofile, JaCoCo, and the reports of cargo-mutants, Stryker, PIT, gremlins and muter). Each reads into the shapes in `src/model.rs`.
- `src/config.rs`: `slimfit.toml` and the per-language presets. A language is just data: commands plus report formats.
- `src/measure.rs`: runs a suite's commands, resolves the reports' paths to repo files, reduces everything to metrics, and caches the result per working tree.
- `src/ratchet.rs`: the baseline, `check` and `ratchet`.
- `src/tasks.rs`: turns a measurement into tasks for `slimfit next`.
- `testdata/<language>/`: one sample project per language, with real reports in `reports/`. Every sample holds the same five situations (see `src/fixtures.rs`).

## Checks

- `./gate.sh` (fast): fmt, clippy, then slimfit checks itself (tests, coverage and time against `.slimfit/baseline.json`).
- `./slow.sh`: the fast gate, plus `slimfit check --mutants` on slimfit itself and `E2E_MUTANTS=1 ./e2e.sh` (the Rust sample by default; `E2E_LANGS` picks others).
- A commit that changes source files carries its mutants measurement: run `slimfit check --mutants`, then `slimfit ratchet`, and commit `.slimfit/baseline.json` with the change. That moves `mutants_at`, so the slow gate mutates only the lines changed since (ADR 0005). A change that only deletes code or tests runs no mutant; say in its commit what the deleted tests killed.
- A new report format or preset change needs real output from the real tool in `testdata/<language>/reports/`, never a hand-written one. Replace absolute paths with `/REPO`.

## Agent skills

### Issue tracker

Issues live in factorium's local tracker (`factorium issue … slimfit`). See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context. See `docs/agents/domain.md`.
