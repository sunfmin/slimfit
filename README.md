# slimfit

Keep a codebase small and its tests few, fast and strong.

slimfit is made for AI coding agents. The agent changes code; slimfit measures each change and only lets the numbers get better:

- **failures** stay at 0
- **test time** (the sum of the tests' own durations) does not grow
- **uncovered lines** do not grow: code may grow, untested code may not
- **surviving mutants** do not grow: a test can be deleted only if the suite still catches everything it caught

`slimfit next` hands out one task at a time, cheapest and safest first: fix failing tests, remove uncovered code, remove redundant tests, speed up slow tests, kill surviving mutants, and deepen modules that keep changing. Each task carries its evidence and the command that accepts it.

The full manual is `slimfit --help`.

## Install

```sh
cargo install --locked --git https://github.com/sunfmin/slimfit
```

## Languages

Each language is a preset: the tools its community uses, each run once to write a standard report. Every preset field can be overridden per suite in `slimfit.toml`.

| Language | Tests and coverage | Mutants | Needs |
|---|---|---|---|
| Rust | cargo-nextest under cargo-llvm-cov (JUnit, LCOV) | cargo-mutants | `cargo install cargo-nextest cargo-llvm-cov cargo-mutants`, `rustup component add llvm-tools-preview` |
| Go | `go test -json -coverprofile` | gremlins | `go install github.com/go-gremlins/gremlins/cmd/gremlins@v0.5.1` |
| TypeScript | vitest with v8 coverage (JUnit, LCOV) | Stryker (`--disableBail`, so every killing test is recorded) | `vitest`, `@vitest/coverage-v8`, `@stryker-mutator/core`, `@stryker-mutator/vitest-runner` in devDependencies |
| Kotlin | Gradle `test` + Kover (JUnit, JaCoCo XML) | PIT with `fullMutationMatrix` | the Kover and `info.solidsoft.pitest` plugins (see `testdata/kotlin/build.gradle.kts`) and a JDK Gradle can use |
| Swift | `swift test` + `llvm-cov export` (xUnit, LCOV) | muter | `brew install muter-mutation-testing/formulae/muter` |

Redundant tests can be found only where the mutation tool records which test kills which mutant: TypeScript (Stryker) and Kotlin (PIT). For the other languages, `slimfit next` says so.

## Use

```sh
slimfit init          # detect suites, write slimfit.toml, measure, write .slimfit/baseline.json
slimfit next          # the next task
slimfit check         # exit 1 if a bound got worse; --mutants also runs mutation testing
slimfit ratchet       # lock the gain into the baseline
slimfit report        # metrics, baseline, and line counts over git history
```

In a repo gated by factorium:

```toml
[gate]
fast = "slimfit check"
slow = "slimfit check --mutants"
```
