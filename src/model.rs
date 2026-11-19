//! What a measurement of one suite holds, in a shape every report format is
//! read into. Paths are relative to the repository root.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestCase {
    /// `classname::name` when the report has a class, else `name`.
    pub id: String,
    /// Its duration as the report gives it: wall time, which a busy host
    /// stretches. A mutant's timeout for the test is set from it (ADR 0007).
    pub seconds: f64,
    pub passed: bool,
    /// The CPU time (user + sys) its own processes spent: what
    /// `test_seconds` and slow-test tasks count (ADR 0010). 0 as a report
    /// is read; the measurement sets it.
    #[serde(default)]
    pub cpu: f64,
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
    /// The tests' CPU time: what it counts is the `test_seconds` bound's
    /// help (`ratchet::Bound`), its one home.
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

/// A mutant of the change, as the suite's tool lists it: what the agent
/// picks from (`check --mutants` with no names).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    /// The tool's name for it: what `check --mutants <name>` picks it by.
    pub name: String,
    pub file: String,
    pub line: u32,
    /// The function it is in; empty when the tool names none.
    pub function: String,
    /// The code it puts in place of the original.
    pub replacement: String,
}

/// What a [`Candidate`] prints, as the command pages name it.
pub const CANDIDATE_HELP: &str = "each with `name` (what a pick names), `file`, `line`, `function` and \
     `replacement` (the code it puts in)";

/// A mutant the agent picked, why, and what its run found.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pick {
    pub name: String,
    pub why: String,
    /// `None` when its run reported no outcome for it.
    pub outcome: Option<MutantStatus>,
}

/// What a [`Pick`] prints, as the command pages name it.
pub const PICK_HELP: &str = "each with `name`, `why` (the reason it was picked) and `outcome` (`killed`, \
     `survived` or `unviable`; `null` when the run reported none)";

/// The picked mutants a run of the change ran, and what they came to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Mutated {
    #[serde(default)]
    pub picks: Vec<Pick>,
    /// The picks by outcome: a survivor here is the change's own, and
    /// `check --mutants` holds them at 0.
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
    /// The picks the mutation run ran; `None` without mutants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutated: Option<Mutated>,
    pub command_seconds: f64,
    /// The test run ran inside a `--mutants` run, at background priority by
    /// the rule: its `test_seconds` is not what a gate measures, so it is
    /// neither checked nor ratcheted. A plain `check` of the tree runs the
    /// tests again and keeps the mutants.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untimed: bool,
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
    /// The source lines each test ran (test code left out), by file, in the
    /// order of `tests`: what redundant-test candidates are found from (ADR
    /// 0006). `None` when the test run records functions only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<BTreeMap<String, Vec<u32>>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReachTest {
    /// The test binary's id: the JUnit report's classname.
    pub binary: String,
    pub name: String,
    /// Where the test's own function is, and a hash of its text: what a
    /// keep verdict is recorded against. `None` when the coverage report
    /// names no function of the test's path, or its end is not found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<TestSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestSource {
    pub file: String,
    /// The line of its `fn`.
    pub line: u32,
    /// FNV-1a 64 of its lines, from its `fn` to its closing brace, in hex.
    pub hash: String,
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
    /// Picked mutants of the change ran (ADR 0008).
    pub with_mutants: bool,
    pub suites: Vec<SuiteMeasurement>,
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
