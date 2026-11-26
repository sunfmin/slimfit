# slimfit

A CLI that keeps a codebase small and its tests few, fast and strong, used by AI agents. `slimfit --help` is their manual, written in `src/cli.rs` and `src/help/`: keep it true whenever behaviour changes. `README.md` says how to build and run it; `docs/adr/` holds the decisions.

## Invariants

- Rust, Go and Python syntax each have one reader: `src/rust_source.rs`, `src/go_source.rs`, `src/python_source.rs`.
- `Baseline::next` is the one place a baseline is built from the one before it.
- A preset change updates `examples/standin/` in the same commit.
- A new report format or preset change needs real output from the real tool in `testdata/<language>/reports/`, never a hand-written one, with absolute paths as `/REPO`.
- Cargo.toml's version is not edited: a release build takes it from the tag (ADR 0013).
- `examples/treecount/` is a measurement, not part of slimfit.

## Checks

- `./gate.sh` is the only gate; `./e2e.sh` runs the real toolchains (`E2E_MUTANTS=1`).
- No gate runs mutation testing on slimfit itself (ADR 0006, 0008). Every change to source has a second agent pick mutants on its decisions, sized to the diff, from `slimfit check --mutants`, runs them with `factorium worker exclusive --background -- slimfit check --mutants <name>... --why <why>...`, then `slimfit ratchet` (or `slimfit ratchet --no-mutants "<why>"`), and the commit carries the record in `.slimfit/mutants/`.
- A surviving pick is killed, or loosened with a reason; that commit carries the ratcheted `.slimfit/baseline.json`.
