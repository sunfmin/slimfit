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
    /// The tests that killed it, when the tool records them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed_by: Option<Vec<String>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutants: Option<MutantCounts>,
    /// `mutants` file by file (they add up to it): what a run that mutates
    /// only the changed files keeps for the files it does not mutate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mutant_files: BTreeMap<String, MutantCounts>,
    /// Source files with no mutant count yet, while a baseline is built in
    /// pieces (`ratchet --pending-files/--pending-seconds`): their mutants
    /// are not in `mutants`, so `check` does not hold them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutants_pending: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
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

    pub fn by_file(mutants: &[Mutant]) -> BTreeMap<String, Self> {
        let mut out: BTreeMap<String, Self> = BTreeMap::new();
        for m in mutants {
            out.entry(m.file.clone()).or_default().add(&Self::of(std::slice::from_ref(m)));
        }
        out
    }

    pub fn sum<'a>(counts: impl IntoIterator<Item = &'a Self>) -> Self {
        let mut c = Self::default();
        for x in counts {
            c.add(x);
        }
        c
    }

    fn add(&mut self, o: &Self) {
        self.killed += o.killed;
        self.survived += o.survived;
        self.unviable += o.unviable;
    }
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
    /// Every test the mutation tool knew of, when it records killers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_tests: Option<Vec<String>>,
    /// The files mutated when only those changed since the baseline were;
    /// `None` when every file was (or none, without mutants).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutated: Option<Vec<String>>,
    pub command_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Measurement {
    /// Identifies the working tree measured: HEAD plus a hash of local changes.
    pub tree: String,
    pub with_mutants: bool,
    /// A piece (`ratchet --pending-seconds`): it filled in pending mutant
    /// counts, so a test_seconds overrun does not refuse its ratchet.
    #[serde(default)]
    pub piece: bool,
    /// The commit measured and the files that differed from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    pub suites: Vec<SuiteMeasurement>,
}

/// A working tree named by git: a commit, and the files (repository
/// relative) whose contents differed from it, untracked ones included.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub commit: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirty: Vec<String>,
}
