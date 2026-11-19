//! `slimfit.toml`: which suites a repository has and how each is measured.
//! A suite names a preset; any field the preset sets can be overridden.

use crate::fault::{Fault, Faults, fault};
use crate::formats::{CoverageFormat, MutantsFormat, ReachFormat, TestsFormat};
use crate::tools::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE: &str = "slimfit.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "suite")]
    pub suites: Vec<SuiteConfig>,
    #[serde(default)]
    pub check: CheckConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckConfig {
    /// `test_seconds` may exceed the baseline by this fraction, plus `time_slack`.
    pub time_tolerance: f64,
    pub time_slack: f64,
    /// Lines by which `uncovered_lines` may move either way and still count
    /// as unchanged: coverage that depends on timing flaps (docs/adr/0001).
    pub coverage_slack: u64,
    /// The branch changes land on: a change's lines are those added since
    /// HEAD's merge base with it (`landing::DEFAULT_TARGET` when unset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self { time_tolerance: 0.2, time_slack: 1.0, coverage_slack: 1, target: None }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Rust,
    Go,
    Swift,
    Kotlin,
    Typescript,
}

impl Preset {
    /// Every preset, in declaration order.
    pub fn all() -> &'static [Preset] {
        <Preset as clap::ValueEnum>::value_variants()
    }

    /// The language's name, as a reader writes it.
    pub fn language(self) -> &'static str {
        match self {
            Preset::Rust => "Rust",
            Preset::Go => "Go",
            Preset::Swift => "Swift",
            Preset::Kotlin => "Kotlin",
            Preset::Typescript => "TypeScript",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteConfig {
    pub name: String,
    pub preset: Preset,
    /// Directory of the suite, relative to the repository root.
    #[serde(default = "dot")]
    pub root: String,
    #[serde(flatten)]
    pub overrides: Overrides,
}

fn dot() -> String {
    ".".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excludes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_report: Option<Report<TestsFormat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_report: Option<Report<CoverageFormat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reach_report: Option<Report<ReachFormat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_report: Option<Report<MutantsFormat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_diff_flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_pick_flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_tests: Option<MutantTests>,
}

/// Which tests a mutant runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MutantTests {
    /// Every test, as the mutation tool runs them.
    All,
    /// Only the tests the test run saw run the mutant's function, each under
    /// a timeout from its own duration (ADR 0007). cargo-mutants under
    /// nextest only, with a `reach_report`; otherwise every test runs.
    Reached,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report<F> {
    pub format: F,
    /// A path or glob relative to the suite root; `$SLIMFIT_OUT` expands to
    /// the suite's scratch directory.
    pub path: String,
}

/// A suite with its preset applied: everything needed to measure it.
#[derive(Debug, Clone, Serialize)]
pub struct Suite {
    pub name: String,
    pub preset: Preset,
    pub root: String,
    pub sources: Vec<String>,
    pub tests: Vec<String>,
    /// Files neither source nor test: vendored, generated, fixtures.
    pub excludes: Vec<String>,
    /// Rust keeps unit tests in `#[cfg(test)] mod` blocks inside source files.
    pub inline_tests: bool,
    /// Builds what `test_command` runs, before it, so the test command's CPU
    /// time is the tests' (ADR 0010). Empty: the test command builds, or
    /// records each test's CPU time itself (the Rust preset).
    pub build_command: String,
    pub test_command: String,
    pub tests_report: Report<TestsFormat>,
    pub coverage_report: Report<CoverageFormat>,
    /// Which tests ran each function, one profile per test; an empty path:
    /// the test run does not record it.
    pub reach_report: Report<ReachFormat>,
    pub mutants_command: String,
    pub mutants_report: Report<MutantsFormat>,
    /// The flag that limits `mutants_command` to the lines a unified diff
    /// adds, given the diff's path (paths in it relative to the suite root):
    /// how `--mutants` runs only the change's mutants (ADR 0006).
    pub mutants_diff_flag: String,
    /// The flag that runs one mutant by name, given once per pick with the
    /// name as an anchored regular expression: how `--mutants <name>...`
    /// runs only the picks (ADR 0008).
    pub mutants_pick_flag: String,
    pub mutants_tests: MutantTests,
}

impl Suite {
    /// Whether its mutation tool lists a change's mutants (`--list --json`
    /// with the diff flag) and runs them by name: no other runs mutants
    /// (ADR 0006, 0008).
    pub fn mutates(&self) -> bool {
        !self.mutants_diff_flag.is_empty() && !self.mutants_pick_flag.is_empty()
    }
}

impl SuiteConfig {
    pub fn resolve(&self) -> Suite {
        let mut s = preset(self.preset);
        let o = self.overrides.clone();
        s.name = self.name.clone();
        s.root = self.root.clone();
        if let Some(v) = o.sources {
            s.sources = v
        }
        if let Some(v) = o.tests {
            s.tests = v
        }
        if let Some(v) = o.excludes {
            s.excludes = v
        }
        if let Some(v) = o.build_command {
            s.build_command = v
        }
        if let Some(v) = o.test_command {
            s.test_command = v
        }
        if let Some(v) = o.tests_report {
            s.tests_report = v
        }
        if let Some(v) = o.coverage_report {
            s.coverage_report = v
        }
        if let Some(v) = o.reach_report {
            s.reach_report = v
        }
        if let Some(v) = o.mutants_command {
            s.mutants_command = v
        }
        if let Some(v) = o.mutants_report {
            s.mutants_report = v
        }
        if let Some(v) = o.mutants_diff_flag {
            s.mutants_diff_flag = v
        }
        if let Some(v) = o.mutants_pick_flag {
            s.mutants_pick_flag = v
        }
        if let Some(v) = o.mutants_tests {
            s.mutants_tests = v
        }
        s
    }
}

/// The Rust preset's test command: a profile per test beside the merged
/// coverage, and which functions each test ran (ADR 0007).
const RUST_TEST: &str = include_str!("presets/rust-test.sh");
/// The Rust preset's mutants command. cargo-mutants builds each mutant in a
/// copy of the tree with no `target`, so its first build compiled every
/// dependency (94-115s on factorium). `cargo llvm-cov show-env` gives the
/// environment the test run built with (its rustc wrapper and flags), and
/// the build goes to the test run's own target directory: a target at the
/// same path, built the same way, keeps every crate but the mutated ones
/// fresh (9-11s; ADR 0007). Without that environment (show-env failed) it
/// builds cold, as before. show-env's `LLVM_PROFILE_FILE` (the test run's
/// target directory) is left out: a mutant's test processes keep the one
/// they had, unset in an agent's shell, so they write their profiles in
/// the copy, which cargo-mutants deletes.
const RUST_MUTANTS: &str = "eval \"$(cargo llvm-cov show-env --sh 2>/dev/null | sed '/^export LLVM_PROFILE_FILE=/d')\"; \
    if [ -n \"${CARGO_LLVM_COV_TARGET_DIR:-}\" ]; then \
    export CARGO_TARGET_DIR=\"$CARGO_LLVM_COV_TARGET_DIR/llvm-cov-target\"; fi; \
    cargo mutants --workspace --test-workspace=true --no-shuffle --test-tool nextest --baseline skip \
    --output \"$SLIMFIT_OUT\"";

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn report<F>(format: F, path: &str) -> Report<F> {
    Report { format, path: path.into() }
}

/// The defaults for each language: the tools each one's community uses,
/// each run once to produce a standard report.
pub fn preset(p: Preset) -> Suite {
    let base = |sources: &[&str], tests: &[&str]| Suite {
        name: String::new(),
        preset: p,
        root: ".".into(),
        sources: strings(sources),
        tests: strings(tests),
        excludes: strings(&[
            "**/testdata/**",
            "**/node_modules/**",
            "**/vendor/**",
            "**/.build/**",
            "**/build/**",
            "**/target/**",
            "**/dist/**",
        ]),
        inline_tests: false,
        build_command: String::new(),
        test_command: String::new(),
        tests_report: report(TestsFormat::Junit, ""),
        coverage_report: report(CoverageFormat::Lcov, "$SLIMFIT_OUT/lcov.info"),
        reach_report: report(ReachFormat::Lcov, ""),
        mutants_command: String::new(),
        mutants_report: report(MutantsFormat::Stryker, ""),
        mutants_diff_flag: String::new(),
        mutants_pick_flag: String::new(),
        mutants_tests: MutantTests::All,
    };
    match p {
        Preset::Rust => Suite {
            inline_tests: true,
            test_command: RUST_TEST
                .replace("{NEXTEST_JUNIT}", NEXTEST_JUNIT)
                .replace("{REACH}", REACH)
                .replace("{TEST_CPU}", TEST_CPU),
            tests_report: report(TestsFormat::Junit, &format!("{NEXTEST_DIR}/{NEXTEST_JUNIT}")),
            reach_report: report(ReachFormat::Lcov, &format!("$SLIMFIT_OUT/{REACH}")),
            // nextest stops a mutant's tests at its first failure: only whether a
            // mutant is caught is read (ADR 0006). No baseline: the test run
            // already showed the tree green (ADR 0007). Each mutant builds as the
            // test run built, into its target directory, so only the mutated
            // crates compile: no cold first build (ADR 0007, slimfit#35).
            mutants_command: RUST_MUTANTS.into(),
            mutants_report: report(MutantsFormat::CargoMutants, &format!("$SLIMFIT_OUT/{CARGO_MUTANTS_OUTCOMES}")),
            mutants_diff_flag: "--in-diff".into(),
            mutants_pick_flag: "-F".into(),
            mutants_tests: MutantTests::Reached,
            ..base(&["**/*.rs"], &["tests/**", "**/tests/**", "benches/**", "**/benches/**", "examples/**"])
        },
        Preset::Go => Suite {
            // Builds the test binaries as the test run does (coverage on), and
            // runs none of their tests. `-count=1`: a cached result spent no CPU.
            build_command: "go test -cover -run '^$' ./... > /dev/null".into(),
            test_command: "go test -count=1 -json -coverprofile=\"$SLIMFIT_OUT/cover.out\" ./... > \"$SLIMFIT_OUT/test.json\"".into(),
            tests_report: report(TestsFormat::GoTestJson, "$SLIMFIT_OUT/test.json"),
            coverage_report: report(CoverageFormat::GoCover, "$SLIMFIT_OUT/cover.out"),
            mutants_command: "gremlins unleash --timeout-coefficient 10 --output \"$SLIMFIT_OUT/gremlins.json\"".into(),
            mutants_report: report(MutantsFormat::Gremlins, "$SLIMFIT_OUT/gremlins.json"),
            ..base(&["**/*.go"], &["**/*_test.go", "**/testdata/**"])
        },
        Preset::Swift => Suite {
            build_command: "swift build --build-tests --enable-code-coverage".into(),
            // XCTest writes xunit only with --parallel; one worker keeps per-test times.
            // .build/debug is a symlink, so find needs the trailing slash.
            test_command: format!(
                "swift test --parallel --num-workers 1 --enable-code-coverage --xunit-output \"$SLIMFIT_OUT/xunit.xml\"; rc=$?; \
                 bin=$(find {SWIFT_DEBUG}/ -maxdepth 1 -name '*.xctest' | head -1); \
                 xcrun llvm-cov export -format=lcov -instr-profile={SWIFT_PROFDATA} \
                 \"$bin/Contents/MacOS/$(basename \"$bin\" .xctest)\" > \"$SLIMFIT_OUT/lcov.info\"; exit $rc"
            ),
            tests_report: report(TestsFormat::Junit, "$SLIMFIT_OUT/xunit*.xml"),
            mutants_command: "[ -f muter.conf.yml ] || muter init; muter run --skip-update-check --format json --output \"$SLIMFIT_OUT/muter.json\"".into(),
            mutants_report: report(MutantsFormat::Muter, "$SLIMFIT_OUT/muter.json"),
            ..base(&["Sources/**/*.swift"], &["Tests/**"])
        },
        Preset::Kotlin => Suite {
            // No daemon: the tests then run in this command's process tree,
            // where their CPU time is counted (ADR 0010).
            build_command: "./gradlew --quiet --no-daemon testClasses".into(),
            test_command: "./gradlew --quiet --no-daemon test koverXmlReport --continue".into(),
            tests_report: report(TestsFormat::Junit, &format!("**/{GRADLE_TEST_RESULTS}/*.xml")),
            coverage_report: report(CoverageFormat::Jacoco, &format!("**/{KOVER_XML}")),
            mutants_command: "./gradlew --quiet pitest".into(),
            mutants_report: report(MutantsFormat::Pitest, &format!("**/{PITEST_XML}")),
            ..base(&["**/src/main/**/*.kt", "**/src/main/**/*.java"], &["**/src/test/**", "**/src/androidTest/**"])
        },
        Preset::Typescript => Suite {
            test_command: "npx vitest run --coverage.enabled --coverage.reporter=lcov \
                --coverage.reportsDirectory=\"$SLIMFIT_OUT/coverage\" --reporter=junit --outputFile=\"$SLIMFIT_OUT/junit.xml\""
                .into(),
            tests_report: report(TestsFormat::Junit, "$SLIMFIT_OUT/junit.xml"),
            coverage_report: report(CoverageFormat::Lcov, "$SLIMFIT_OUT/coverage/lcov.info"),
            mutants_command: "npx stryker run --reporters json --disableBail".into(),
            mutants_report: report(MutantsFormat::Stryker, STRYKER_JSON),
            ..base(
                &["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.mts"],
                &["**/*.test.*", "**/*.spec.*", "**/__tests__/**", "test/**", "tests/**", "**/*.config.*", "**/*.d.ts"],
            )
        },
    }
}

const MARKERS: &[(&str, Preset)] = &[
    ("Cargo.toml", Preset::Rust),
    ("go.mod", Preset::Go),
    ("Package.swift", Preset::Swift),
    ("settings.gradle.kts", Preset::Kotlin),
    ("settings.gradle", Preset::Kotlin),
    ("build.gradle.kts", Preset::Kotlin),
    ("tsconfig.json", Preset::Typescript),
];

/// One suite per project root found among `files`: the shallowest marker of
/// each language wins, and nothing nested under a found root of the same
/// language starts a second suite.
pub fn detect(files: &[String]) -> Vec<SuiteConfig> {
    let mut found: Vec<(String, Preset)> = Vec::new();
    let mut candidates: Vec<(&str, Preset)> = files
        .iter()
        .filter_map(|f| {
            let (dir, name) = f.rsplit_once('/').unwrap_or(("", f));
            MARKERS.iter().find(|(m, _)| *m == name).map(|(_, p)| (dir, *p))
        })
        .filter(|(dir, _)| !dir.split('/').any(|c| c == "node_modules" || c == "testdata"))
        .collect();
    candidates.sort_by_key(|(dir, _)| (dir.matches('/').count() + usize::from(!dir.is_empty()), dir.to_string()));
    for (dir, p) in candidates {
        let nested = found
            .iter()
            .any(|(root, q)| *q == p && (root.is_empty() || dir == root || dir.starts_with(&format!("{root}/"))));
        if !nested {
            found.push((dir.to_string(), p));
        }
    }
    found
        .into_iter()
        .map(|(dir, p)| {
            let lang = toml::Value::try_from(p).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
            SuiteConfig {
                name: if dir.is_empty() { lang } else { dir.replace('/', "-") },
                preset: p,
                root: if dir.is_empty() { ".".into() } else { dir },
                overrides: Overrides::default(),
            }
        })
        .collect()
}

/// `slimfit.toml`, or every fault found in it.
pub fn load(repo: &Path) -> Result<Config> {
    let path = repo.join(FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        fault(
            format!("cannot read {} ({e})", path.display()),
            "`slimfit init` writes it (or restore it from git: `git checkout -- slimfit.toml`)",
        )
    })?;
    parse(repo, &text)
}

/// `slimfit.toml`'s text, or every fault found in it; a suite's root is
/// looked for in `repo`.
pub fn parse(repo: &Path, text: &str) -> Result<Config> {
    let cfg: Config = toml::from_str(text).map_err(|e| {
        fault(
            format!("{FILE} is not valid: {}", e.to_string().trim()),
            format!(
                "edit {FILE} where the fault points; each [[suite]] needs name and preset (rust, go, swift, \
                     kotlin or typescript), and may set root"
            ),
        )
    })?;
    let mut faults = Vec::new();
    if cfg.suites.is_empty() {
        faults.push(Fault {
            fault: format!("{FILE} names no [[suite]]"),
            fix: format!("add one: `[[suite]]` with name, preset and root, or delete {FILE} and run `slimfit init`"),
        });
    }
    for (i, s) in cfg.suites.iter().enumerate() {
        // Once per name: at its second suite.
        if cfg.suites[..i].iter().filter(|t| t.name == s.name).count() == 1 {
            faults.push(Fault {
                fault: format!("{FILE}: more than one [[suite]] is named {:?}", s.name),
                fix: format!("rename all but one of the suites named {:?}: each needs its own name", s.name),
            });
        }
        if !repo.join(&s.root).is_dir() {
            faults.push(Fault {
                fault: format!("{FILE}: suite {:?} has root {:?}, which is no directory", s.name, s.root),
                fix: format!("set root of suite {:?} to its directory, relative to the repository", s.name),
            });
        }
    }
    if faults.is_empty() { Ok(cfg) } else { Err(Faults(faults).into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(v: &[&str]) -> Vec<String> {
        strings(v)
    }

    #[test]
    fn detects_one_suite_per_language_root() {
        let found = detect(&files(&[
            "Cargo.toml",
            "crates/a/Cargo.toml",
            "web/tsconfig.json",
            "web/node_modules/x/tsconfig.json",
            "svc/go.mod",
            "testdata/go/go.mod",
        ]));
        let got: Vec<(&str, &str, Preset)> =
            found.iter().map(|s| (s.name.as_str(), s.root.as_str(), s.preset)).collect();
        assert_eq!(
            got,
            vec![("rust", ".", Preset::Rust), ("svc", "svc", Preset::Go), ("web", "web", Preset::Typescript)]
        );
    }

    /// Every fault of slimfit.toml at once, each with its fix.
    #[test]
    fn load_names_every_fault_with_its_fix() {
        let dir = tempfile::tempdir().unwrap();
        let faults = |text: Option<&str>| {
            if let Some(t) = text {
                std::fs::write(dir.path().join(FILE), t).unwrap();
            }
            crate::fault::of(load(dir.path()).unwrap_err())
        };
        let f = faults(None);
        assert!(f[0].fault.starts_with("cannot read") && f[0].fix.starts_with("`slimfit init` writes it"), "{f:?}");
        let f = faults(Some("[[suite]]\nname = \"a\"\npreset = \"cobol\"\n"));
        assert!(f[0].fault.starts_with("slimfit.toml is not valid") && f[0].fault.contains("cobol"), "{f:?}");
        assert!(f[0].fix.contains("preset (rust, go, swift, kotlin or typescript)"), "{f:?}");
        let f = faults(Some("suite = []\n"));
        assert!(f[0].fix.starts_with("add one: `[[suite]]`"), "{f:?}");
        let s = |name: &str, root: &str| format!("[[suite]]\nname = \"{name}\"\npreset = \"go\"\nroot = \"{root}\"\n");
        let f = faults(Some(&[s("a", "."), s("a", "."), s("a", "."), s("b", "gone")].concat()));
        let fixes: Vec<&str> = f.iter().map(|f| f.fix.as_str()).collect();
        assert_eq!(
            fixes,
            [
                "rename all but one of the suites named \"a\": each needs its own name",
                "set root of suite \"b\" to its directory, relative to the repository"
            ]
        );
        std::fs::write(dir.path().join(FILE), s("a", ".")).unwrap();
        assert_eq!(load(dir.path()).unwrap().suites.len(), 1);
    }

    #[test]
    fn overrides_replace_preset_fields() {
        let s: Config = toml::from_str(
            r#"
            [[suite]]
            name = "core"
            preset = "go"
            root = "core"
            test_command = "make test"
            reach_report = { format = "lcov", path = "reach.txt" }
            mutants_tests = "all"
            "#,
        )
        .unwrap();
        let r = s.suites[0].resolve();
        assert_eq!(r.test_command, "make test");
        assert_eq!((r.reach_report.path.as_str(), r.mutants_tests), ("reach.txt", MutantTests::All));
        assert_eq!(r.tests_report.format, TestsFormat::GoTestJson);
        assert_eq!(r.root, "core");
    }
}
