//! Readers for the report files test, coverage and mutation tools write.
//! Each reads one standard format into the shapes of `model`; file paths
//! come back as the tool wrote them, and `measure` resolves them.

mod coverage;
mod mutants;
mod reach;
mod tests;
mod xml;

pub use coverage::read as read_coverage;
pub use mutants::{list as read_mutants_list, read as read_mutants};
pub use reach::{TestReach, read as read_reach};
pub use tests::read as read_tests;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TestsFormat {
    /// JUnit XML: nextest, vitest, jest, gradle, `swift test --xunit-output`.
    Junit,
    /// `go test -json`.
    GoTestJson,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CoverageFormat {
    /// LCOV tracefile: cargo llvm-cov, llvm-cov export, istanbul/v8.
    Lcov,
    /// `go test -coverprofile`.
    GoCover,
    /// JaCoCo XML: jacoco, kover.
    Jacoco,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MutantsFormat {
    /// cargo-mutants `mutants.out/outcomes.json`.
    CargoMutants,
    /// Stryker's mutation-testing-report-schema JSON.
    Stryker,
    /// PIT `mutations.xml`.
    Pitest,
    /// gremlins `--output` JSON.
    Gremlins,
    /// muter `--format json`.
    Muter,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReachFormat {
    /// `test <binary>\t<name>` lines, each followed by the LCOV of what that
    /// test's profiles ran (`llvm-cov export -format=lcov`, records of counts
    /// above 0 only): the Rust preset's test command writes it (ADR 0007).
    Lcov,
    /// `test <package>\t<name>` lines, each followed by the blocks of that
    /// test's own Go coverprofile (counts above 0 only): the Go preset's
    /// test command writes it, running each top-level test on its own.
    GoCover,
}
