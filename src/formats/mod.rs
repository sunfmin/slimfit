//! Readers for the report files test, coverage and mutation tools write.
//! Each reads one standard format into the shapes of `model`; file paths
//! come back as the tool wrote them, and `measure` resolves them.

mod coverage;
mod mutants;
mod tests;
mod xml;

pub use coverage::read as read_coverage;
pub use mutants::{MutationRun, read as read_mutants};
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
