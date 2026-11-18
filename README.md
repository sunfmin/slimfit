# slimfit

Keep a codebase small and its tests few, fast and strong.

slimfit is made for AI coding agents. The agent changes code; slimfit measures each change and only lets the numbers get better:

- **failures** stay at 0
- **test time** (the sum of the tests' own durations) does not grow
- **uncovered lines** do not grow: code may grow, untested code may not
- **surviving mutants**: none in the lines a change adds; every mutant of new or edited code is killed. Mutants run on the change only, so adopting slimfit costs no mutation run

`slimfit next` hands out one task at a time, cheapest and safest first: fix failing tests, remove uncovered code, remove redundant tests, speed up slow tests, kill surviving mutants, and deepen modules that keep changing. Each task carries its evidence and the command that accepts it.

The manual is `slimfit --help`, and it is the only copy: how to place slimfit in a gate, what each command costs, and the loop are there, not here. Read its first screen before adopting slimfit on a repo. It leads with what matters most:

```sh
slimfit --help | sed -n '/^1\./,/^WHAT EACH/p'
```

## Install

```sh
cargo install --locked --git https://github.com/sunfmin/slimfit
```

## Languages

Each language is a preset: the tools its community uses, each run once to write a standard report. Every preset field can be overridden per suite in `slimfit.toml`.

| Language | Tests and coverage | Mutants | Needs |
|---|---|---|---|
| Rust | cargo-nextest under cargo-llvm-cov (JUnit, LCOV) | cargo-mutants running nextest with `--no-fail-fast`, so each mutant's log names every killing test: `--mutants` runs only the mutants of the lines changed since the baseline's `mutants_at` (`--in-diff`); `next --all-mutants` runs every mutant | `cargo install cargo-nextest cargo-llvm-cov cargo-mutants`, `rustup component add llvm-tools-preview` |
| Go | `go test -json -coverprofile` | gremlins | `go install github.com/go-gremlins/gremlins/cmd/gremlins@v0.5.1` |
| TypeScript | vitest with v8 coverage (JUnit, LCOV) | Stryker (`--disableBail`, so every killing test is recorded) | `vitest`, `@vitest/coverage-v8`, `@stryker-mutator/core`, `@stryker-mutator/vitest-runner` in devDependencies |
| Kotlin | Gradle `test` + Kover (JUnit, JaCoCo XML) | PIT with `fullMutationMatrix` | the Kover and `info.solidsoft.pitest` plugins (see `testdata/kotlin/build.gradle.kts`) and a JDK Gradle can use |
| Swift | `swift test` + `llvm-cov export` (xUnit, LCOV) | muter | `brew install muter-mutation-testing/formulae/muter` |

`--mutants` measures the change: the lines that differ from the tree the baseline's `mutants_at` names, and none of their mutants may survive. Rust's tool runs only those lines' mutants; every other language's runs every mutant, and slimfit holds only the survivors in the changed lines. No mutant count is kept, so nothing has to be built first. See `docs/adr/0005-mutants-of-the-change.md`.

Redundant tests can be found only where the mutation tool records which test kills which mutant: Rust (cargo-mutants under nextest, from each mutant's log), TypeScript (Stryker) and Kotlin (PIT). For the other languages, `slimfit next` says so. Recording every killer means running every test on every killed mutant instead of stopping at the first failure; `docs/adr/0004-rust-kill-matrix.md` says what that costs.

## Develop

- `./gate.sh`: the fast gate (fmt, clippy, slimfit checking itself).
- `./slow.sh`: the fast gate and the Rust sample end to end. `E2E_LANGS="go swift" ./slow.sh` runs those languages instead; `E2E_LANGS=all ./slow.sh` runs every language whose toolchain is present; `E2E_MUTANTS=1` adds the samples' mutants e2e.
- No gate and no commit runs mutation testing on slimfit itself (mutants are never in a gate). slimfit's own mutants run as idle work: `factorium.toml` `[idle]` runs `slimfit next --all-mutants` once a day while the host is idle. `slimfit next --mutants` is for working surviving-mutant and redundant-test tasks.
- `./e2e.sh [rust|go|typescript|kotlin|swift]...`: end to end on its own; no arguments means every language.
