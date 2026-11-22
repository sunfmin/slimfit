# slimfit

A CLI that keeps a codebase small and its tests few, fast and strong. It is used by AI agents: they change code, and slimfit measures the change and ratchets the result. `slimfit --help` is the agents' manual. Keep it true whenever behaviour changes.

## Layout

- `src/cli.rs`: the command line, read against the manual: the commands and flags with their pages, the topics, the placeholders' values, and the exit codes. `cli::read` answers help itself and turns a line clap refuses into a fault. `src/main.rs` runs the command it reads and prints the answer.
- `src/formats/`: readers for the report files the tools write (JUnit, go test JSON, LCOV, Go coverprofile, JaCoCo, and the reports of cargo-mutants, Stryker, PIT, gremlins and muter). Each reads into the shapes in `src/model.rs`.
- `src/config.rs`: `slimfit.toml` and the per-language presets. A language is just data: commands plus report formats.
- `src/measure.rs`: runs a suite's commands, resolves the reports' paths to repo files, reduces everything to metrics, and caches the result per working tree. `src/measure/mutants.rs`: the change's mutants (ADR 0006-0008): what each tool lists of the change, the picks among them, and their run.
- `src/growth.rs`: a change's growth (ADR 0012): the source lines it adds net, and the growth record a second agent's verdict on it writes.
- `src/session.rs`: what every measuring command opens first: config, baseline and HEAD's tree (`Session::open`), then the tree's measurement (`Session::measure`).
- `src/ratchet.rs`: the baseline, `check` and `ratchet`; `Baseline::next` is the one place a baseline is built from the one before it.
- `src/tasks.rs`: turns a measurement into tasks for `slimfit next`.
- `src/select.rs`: which tests each mutant runs (ADR 0007): the plan, and the shim slimfit becomes when run as `cargo-nextest`.
- `src/rust_source.rs`: the one reader of Rust syntax: where an item ends, `fn`s and their names, `#[cfg(test)]` items and modules, const-ness. `loc`, `measure` and `select` ask it.
- `src/go_source.rs`: the one reader of Go syntax: where a top-level test `func` is and its text. `measure` asks it.
- `src/python_source.rs`: the one reader of Python syntax: where a pytest test `def` is and its text. `measure` asks it.
- `src/presets/`: preset commands too long for a Rust string (the Rust test command and its per-test runner; the Go test command and its run of each test; the Python test command, and the mutmut driver that takes cargo-mutants' flags and writes its shapes).
- `build.rs` + `src/version.rs`: stamp `slimfit --version`. A release (`packaging/release.sh cut`) sets `SLIMFIT_RELEASE` from the tag; Cargo.toml's version is not edited.
- `testdata/<language>/`: one sample project per language, with real reports in `reports/`. Every sample holds the same five situations (see `src/fixtures.rs`).
- `src/tools.rs`: where the tools write reports on their own, shared with the stand-ins.
- `examples/treecount/`: slimfit#66's measurement, not part of slimfit: instructions retired over each test's whole process tree (a hook injected into every process, system binaries that run others run as re-signed copies), as nextest's runner. `tests/treecount.rs` holds that a child's work is counted.
- `examples/standin/`: one stand-in for every tool the presets drive, linked under each tool's name first on PATH. It takes arguments as the real tool does, writes the sample's real reports, records each call and injects faults, all in one JSON state file. `tests/standins.rs` runs the real binary through it on each sample. A preset change updates the stand-in in the same commit.

## Checks

- `./gate.sh` (fast): fmt, clippy, then slimfit checks itself (tests and the mutants record; coverage and time as trends against `.slimfit/baseline.json`).
- `./slow.sh`: the fast gate; `E2E_LANGS` adds `./e2e.sh` on those languages' real toolchains (the Python sample's mutants of a change always; `E2E_MUTANTS=1` adds the Rust sample's, on request).
- No gate and no commit runs mutation testing on slimfit itself: mutants are never in a gate (`slimfit --help`, rule 2), and run only on a change, picked: nothing mutates the whole codebase (ADR 0006), nor every mutant of a change (ADR 0008). Every change to source picks its mutants before it lands: `slimfit check --mutants` lists them; pick at most 5 on the decisions the change makes and run them with `factorium worker exclusive --background -- slimfit check --mutants <name>... --why <why>...`, then `slimfit ratchet`, and the commit carries the record it writes in `.slimfit/mutants/` (or `slimfit ratchet --no-mutants "<why>"` when nothing is worth one); the gate's plain `slimfit check` fails without a record (ADR 0008). A surviving pick is killed, or loosened with a reason, and that commit carries the ratcheted `.slimfit/baseline.json`.
- A new report format or preset change needs real output from the real tool in `testdata/<language>/reports/`, never a hand-written one. Replace absolute paths with `/REPO`.

## Agent skills

### Issue tracker

Issues live in factorium's local tracker (`factorium issue … slimfit`). See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context. See `docs/agents/domain.md`.
