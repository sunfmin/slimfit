//! The stand-ins' state: one JSON file, named by `ENV`, shared by every
//! stand-in of one test. The test writes the reports to copy and the
//! faults to inject; each stand-in appends the call it got.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The environment variable that names the state file.
pub const ENV: &str = "SLIMFIT_STANDIN_STATE";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// The real reports a stand-in copies, as `testdata/<language>/reports`
    /// holds them (`/REPO` for the project root).
    pub reports: PathBuf,
    /// Faults to inject, by step (`cargo llvm-cov`, `gradlew pitest`, ...).
    #[serde(default)]
    pub faults: BTreeMap<String, Fault>,
    #[serde(default)]
    pub calls: Vec<Call>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct Fault {
    /// Exit with this status, after writing the report unless `no_report`.
    #[serde(default)]
    pub exit: i32,
    /// Write no report.
    #[serde(default)]
    pub no_report: bool,
    /// nextest: the first test it runs fails.
    #[serde(default)]
    pub first_test_fails: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Call {
    /// The tool and its subcommand, as faults are keyed.
    pub step: String,
    pub args: Vec<String>,
    /// The directory it ran in.
    pub dir: PathBuf,
    /// `CARGO_TARGET_DIR`, when set: where cargo would build.
    #[serde(default)]
    pub target_dir: Option<PathBuf>,
}

impl State {
    pub fn load(path: &Path) -> State {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("standin state {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("standin state {}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) {
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap()).unwrap();
    }
}
