# slimfit

A CLI that keeps a codebase small and its tests few, fast and strong. It is used by AI agents: they change code, and slimfit measures the change and ratchets the result. `slimfit --help` is the agents' manual. Keep it true whenever behaviour changes.

## Layout

- `src/formats/`: readers for the report files the tools write (JUnit, go test JSON, LCOV, Go coverprofile, JaCoCo, and the reports of cargo-mutants, Stryker, PIT, gremlins and muter). Each reads into the shapes in `src/model.rs`.
- `src/config.rs`: `slimfit.toml` and the per-language presets. A language is just data: commands plus report formats.
- `src/measure.rs`: runs a suite's commands, resolves the reports' paths to repo files, reduces everything to metrics, and caches the result per working tree.
- `src/ratchet.rs`: the baseline, `check` and `ratchet`.
- `src/tasks.rs`: turns a measurement into tasks for `slimfit next`.
- `build.rs` + `src/version.rs`: stamp `slimfit --version`. A release (`packaging/release.sh cut`) sets `SLIMFIT_RELEASE` from the tag; Cargo.toml's version is not edited.
- `testdata/<language>/`: one sample project per language, with real reports in `reports/`. Every sample holds the same five situations (see `src/fixtures.rs`).
- `src/tools.rs`: where the tools write reports on their own, shared with the stand-ins.
- `examples/standin/`: one stand-in for every tool the presets drive, linked under each tool's name first on PATH. It takes arguments as the real tool does, writes the sample's real reports, records each call and injects faults, all in one JSON state file. `tests/standins.rs` runs the real binary through it on each sample. A preset change updates the stand-in in the same commit.

## Checks

- `./gate.sh` (fast): fmt, clippy, then slimfit checks itself (tests, coverage and time against `.slimfit/baseline.json`).
- `./slow.sh`: the fast gate; `E2E_LANGS` adds `./e2e.sh` on those languages' real toolchains (`E2E_MUTANTS=1` adds the samples' mutants e2e, on request).
- No gate and no commit runs mutation testing on slimfit itself: mutants are never in a gate (`slimfit --help`, rule 2). They run as idle work (`factorium.toml` `[idle]`: `slimfit check --all-mutants` once a day while the host is idle). `slimfit next --mutants` is for working surviving-mutant and redundant-test tasks; such a task is accepted with its own accept command, and that commit carries the ratcheted `.slimfit/baseline.json`.
- A new report format or preset change needs real output from the real tool in `testdata/<language>/reports/`, never a hand-written one. Replace absolute paths with `/REPO`.

## Agent skills

### Issue tracker

Issues live in factorium's local tracker (`factorium issue … slimfit`). See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context. See `docs/agents/domain.md`.
