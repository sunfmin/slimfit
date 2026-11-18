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
| Rust | cargo-nextest under cargo-llvm-cov (JUnit, LCOV) | cargo-mutants running nextest with `--no-fail-fast`, so each mutant's log names every killing test: `--mutants` mutates only the files changed since the baseline's commit (`--file`, whole files); `--all-mutants` mutates every file | `cargo install cargo-nextest cargo-llvm-cov cargo-mutants`, `rustup component add llvm-tools-preview` |
| Go | `go test -json -coverprofile` | gremlins | `go install github.com/go-gremlins/gremlins/cmd/gremlins@v0.5.1` |
| TypeScript | vitest with v8 coverage (JUnit, LCOV) | Stryker (`--disableBail`, so every killing test is recorded) | `vitest`, `@vitest/coverage-v8`, `@stryker-mutator/core`, `@stryker-mutator/vitest-runner` in devDependencies |
| Kotlin | Gradle `test` + Kover (JUnit, JaCoCo XML) | PIT with `fullMutationMatrix` | the Kover and `info.solidsoft.pitest` plugins (see `testdata/kotlin/build.gradle.kts`) and a JDK Gradle can use |
| Swift | `swift test` + `llvm-cov export` (xUnit, LCOV) | muter | `brew install muter-mutation-testing/formulae/muter` |

## Develop

- `./gate.sh`: the fast gate (fmt, clippy, slimfit checking itself). Its tests run every preset end to end against stand-ins for the tools (`tests/standins.rs`, `examples/standin/`), so they need no toolchain but Rust.
- `./slow.sh`: the fast gate. `E2E_LANGS="rust go" ./slow.sh` also runs those languages' real toolchains end to end; `E2E_LANGS=all ./slow.sh` every language whose toolchain is present; `E2E_MUTANTS=1` adds the samples' mutants e2e.
- No gate and no commit runs mutation testing on slimfit itself (mutants are never in a gate). slimfit's own mutants run as idle work: `factorium.toml` `[idle]` runs `slimfit check --all-mutants` once a day while the host is idle. `slimfit next --mutants` is for working surviving-mutant and redundant-test tasks.
- `./e2e.sh [rust|go|typescript|kotlin|swift]...`: the real toolchains end to end on their own; no arguments means every language.

The design decisions are in `docs/adr/`.
