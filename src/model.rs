//! What a measurement of one suite holds, in a shape every report format is
//! read into. Paths are relative to the repository root.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestCase {
    /// `classname::name` when the report has a class, else `name`.
    pub id: String,
    pub seconds: f64,
    pub passed: bool,
    /// When it started, in seconds since the Unix epoch, when the report says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
}

/// Line hits per file, plus the functions the report names (some formats
/// name none).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Coverage {
    pub lines: BTreeMap<String, BTreeMap<u32, u64>>,
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Function {
    pub file: String,
    pub name: String,
    pub line: u32,
    pub hits: u64,
    /// The name as the report gives it (a Rust symbol, mangled), when it
    /// differs from `name`: what a test's profile names the function by.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub symbol: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MutantStatus {
    /// A test failed, or timed out, on the mutant.
    Killed,
    /// Every test passed on the mutant, or none ran over it.
    Survived,
    /// The mutant did not build or was skipped; it says nothing about tests.
    Unviable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mutant {
    pub file: String,
    pub line: u32,
    pub description: String,
    pub status: MutantStatus,
}

/// The numbers `check` compares against the baseline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Metrics {
    pub prod_lines: u64,
    pub test_lines: u64,
    pub tests: u64,
    pub failures: u64,
    /// Sum of the tests' own durations: independent of build time and of
    /// how many run in parallel.
    pub test_seconds: f64,
    pub covered_lines: u64,
    pub uncovered_lines: u64,
}

/// What a suite's [`Metrics`] print, as the command pages name them.
pub const METRICS_HELP: &str = "`prod_lines` and `test_lines` (lines of code), `tests`, `failures`, `test_seconds`, `covered_lines` and `uncovered_lines`";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutantCounts {
    pub killed: u64,
    pub survived: u64,
    pub unviable: u64,
}

impl MutantCounts {
    pub fn of(mutants: &[Mutant]) -> Self {
        let mut c = Self::default();
        for m in mutants {
            match m.status {
                MutantStatus::Killed => c.killed += 1,
                MutantStatus::Survived => c.survived += 1,
                MutantStatus::Unviable => c.unviable += 1,
            }
        }
        c
    }
}

/// What a mutation run of the change covered, and what it found there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mutated {
    /// The source files with lines in the change.
    pub files: Vec<String>,
    /// The mutants in the change's lines, by outcome: survivors here are the
    /// change's own, and `check --mutants` holds them at 0.
    pub change: MutantCounts,
}

/// A run of uncovered lines in one file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub file: String,
    pub start: u32,
    pub end: u32,
    pub lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SuiteMeasurement {
    pub suite: String,
    pub metrics: Metrics,
    pub tests: Vec<TestCase>,
    pub uncovered: Vec<Span>,
    /// Empty when mutants were not run.
    pub mutants: Vec<Mutant>,
    /// What each test ran, when the test run records coverage per test (the
    /// Rust preset): the one per-test record, which each mutant's tests are
    /// chosen from (ADR 0007) and redundant tests are proposed from (ADR 0006).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<Reach>,
    /// What the mutation run covered; `None` without mutants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutated: Option<Mutated>,
    pub command_seconds: f64,
    /// How much the host's load slowed the test command: the tests'
    /// durations are divided by it (ADR 0003).
    pub load_slowdown: f64,
}

/// Which tests ran which functions, from a test run that gives every test a
/// coverage profile of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Reach {
    /// Each test, as nextest names it.
    pub tests: Vec<ReachTest>,
    /// Per source file, per function's first line (as the coverage report
    /// names it), the tests that ran it, by index into `tests`. Every
    /// function the coverage report names is here, those no test ran with
    /// none. A test whose profile could not be read counts as running every
    /// function.
    pub functions: BTreeMap<String, BTreeMap<u32, Vec<u32>>>,
    /// The source lines each test ran, by file, in the order of `tests`:
    /// what redundant-test candidates are found from (ADR 0006). `None`
    /// until the test run records lines (slimfit#32).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<BTreeMap<String, Vec<u32>>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReachTest {
    /// The test binary's id: the JUnit report's classname.
    pub binary: String,
    pub name: String,
}

impl ReachTest {
    /// The id the tests report gives it.
    pub fn id(&self) -> String {
        format!("{}::{}", self.binary, self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Measurement {
    /// Identifies the working tree measured: HEAD plus a hash of local changes.
    pub tree: String,
    pub with_mutants: bool,
    /// With mutants: the tree measured, which `ratchet` records as the
    /// baseline's `mutants_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    /// With mutants: the baseline's `mutants_at` the change was taken from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Snapshot>,
    pub suites: Vec<SuiteMeasurement>,
}

/// A working tree named by git: HEAD, and the tree object its files
/// (untracked ones included) were written to. The tree is unreachable, so
/// another clone, or `git gc` after a while, loses it; the commit stays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each status counts in its own field.
    #[test]
    fn mutant_counts_count_each_status() {
        let m = |status| Mutant { file: "a".into(), line: 1, description: String::new(), status };
        use MutantStatus::*;
        let all = [m(Killed), m(Unviable), m(Unviable), m(Survived), m(Killed), m(Killed)];
        assert_eq!(MutantCounts::of(&all), MutantCounts { killed: 3, survived: 1, unviable: 2 });
    }
}
