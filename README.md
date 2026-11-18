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

Mutants run only on a change: `--mutants` runs the mutants of the lines changed since the baseline's `mutants_at`, and nothing mutates the whole codebase (ADR 0006). A mutants tool that can be limited neither to a diff nor to files (`mutants_diff_flag`, `mutants_file_flag`) is refused by `--mutants`; today that is every preset's but Rust's.

| Language | Tests and coverage | Mutants | Needs |
|---|---|---|---|
| Rust | cargo-nextest under cargo-llvm-cov (JUnit, LCOV), a coverage profile per test (which functions each test ran) | cargo-mutants running nextest, given the change's diff (`--in-diff`), with no baseline run; each mutant runs only the tests that ran its function, each under its own timeout (ADR 0007) | `cargo install cargo-nextest cargo-llvm-cov cargo-mutants`, `rustup component add llvm-tools-preview` |
| Go | `go test -json -coverprofile` | gremlins (no diff or file flag set) | `go install github.com/go-gremlins/gremlins/cmd/gremlins@v0.5.1` |
| TypeScript | vitest with v8 coverage (JUnit, LCOV) | Stryker (no diff or file flag set) | `vitest`, `@vitest/coverage-v8`, `@stryker-mutator/core`, `@stryker-mutator/vitest-runner` in devDependencies |
| Kotlin | Gradle `test` + Kover (JUnit, JaCoCo XML) | PIT (no diff or file flag set) | the Kover and `info.solidsoft.pitest` plugins (see `testdata/kotlin/build.gradle.kts`) and a JDK Gradle can use |
| Swift | `swift test` + `llvm-cov export` (xUnit, LCOV) | muter (no diff or file flag set) | `brew install muter-mutation-testing/formulae/muter` |

## Develop

- `./gate.sh`: the fast gate (fmt, clippy, slimfit checking itself). Its tests run every preset end to end against stand-ins for the tools (`tests/standins.rs`, `examples/standin/`), so they need no toolchain but Rust.
- `./slow.sh`: the fast gate. `E2E_LANGS="rust go" ./slow.sh` also runs those languages' real toolchains end to end; `E2E_LANGS=all ./slow.sh` every language whose toolchain is present; `E2E_MUTANTS=1` adds the Rust sample's mutants of a change.
- No gate and no commit runs mutation testing on slimfit itself (mutants are never in a gate), and nothing runs every mutant of it. `slimfit check --mutants` measures a change's mutants on request; `slimfit next --mutants` gives the surviving-mutant tasks of a change.
- `./e2e.sh [rust|go|typescript|kotlin|swift]...`: the real toolchains end to end on their own; no arguments means every language.

The design decisions are in `docs/adr/`.
