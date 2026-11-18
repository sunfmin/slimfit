# slimfit

Keep a codebase small and its tests few, fast and strong.

slimfit is made for AI coding agents: the agent changes code, slimfit measures each change and only lets the numbers get better.

The manual is `slimfit --help`, and it is the only copy: read it before adopting slimfit on a repo.

## Install

```sh
cargo install --locked --git https://github.com/sunfmin/slimfit
```

## Languages

Each language is a preset: the tools its community uses, each run once to write a standard report. Every preset field can be overridden per suite in `slimfit.toml`.

| Language | Tests and coverage | Mutants | Needs |
|---|---|---|---|
| Rust | cargo-nextest under cargo-llvm-cov (JUnit, LCOV) | cargo-mutants running nextest with `--no-fail-fast`, so each mutant's log names every killing test: ## Develop

- `./gate.sh`: the fast gate (fmt, clippy, slimfit checking itself).
- `./slow.sh`: the fast gate, slimfit's own mutants, and the Rust sample end to end. `E2E_LANGS="go swift" ./slow.sh` runs those languages instead; `E2E_LANGS=all ./slow.sh` runs every language whose toolchain is present.
- `./e2e.sh [rust|go|typescript|kotlin|swift]...`: end to end on its own; no arguments means every language.

The design decisions are in `docs/adr/`.
