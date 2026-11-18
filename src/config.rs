//! `slimfit.toml`: which suites a repository has and how each is measured.
//! A suite names a preset; any field the preset sets can be overridden.

use crate::formats::{CoverageFormat, MutantsFormat, TestsFormat};
use anyhow::{Context, Result, bail};
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
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self { time_tolerance: 0.2, time_slack: 1.0, coverage_slack: 1 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Rust,
    Go,
    Swift,
    Kotlin,
    Typescript,
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
    pub test_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_report: Option<Report<TestsFormat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_report: Option<Report<CoverageFormat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_report: Option<Report<MutantsFormat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutants_file_flag: Option<String>,
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
    pub test_command: String,
    pub tests_report: Report<TestsFormat>,
    pub coverage_report: Report<CoverageFormat>,
    pub mutants_command: String,
    pub mutants_report: Report<MutantsFormat>,
    /// The flag that limits `mutants_command` to one file, given once per
    /// file (paths relative to the suite root), for `--mutants` to mutate
    /// only the files changed since the baseline. Empty: the tool has none,
    /// and every file is mutated.
    pub mutants_file_flag: String,
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
        if let Some(v) = o.test_command {
            s.test_command = v
        }
        if let Some(v) = o.tests_report {
            s.tests_report = v
        }
        if let Some(v) = o.coverage_report {
            s.coverage_report = v
        }
        if let Some(v) = o.mutants_command {
            s.mutants_command = v
        }
        if let Some(v) = o.mutants_report {
            s.mutants_report = v
        }
        if let Some(v) = o.mutants_file_flag {
            s.mutants_file_flag = v
        }
        s
    }
}

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
        test_command: String::new(),
        tests_report: report(TestsFormat::Junit, ""),
        coverage_report: report(CoverageFormat::Lcov, "$SLIMFIT_OUT/lcov.info"),
        mutants_command: String::new(),
        mutants_report: report(MutantsFormat::Stryker, ""),
        mutants_file_flag: String::new(),
    };
    match p {
        Preset::Rust => Suite {
            inline_tests: true,
            test_command: "printf '[profile.default.junit]\\npath = \"junit.xml\"\\n' > \"$SLIMFIT_OUT/nextest.toml\" && \
                cargo llvm-cov nextest --workspace --tool-config-file \"slimfit:$SLIMFIT_OUT/nextest.toml\" --lcov --output-path \"$SLIMFIT_OUT/lcov.info\""
                .into(),
            tests_report: report(TestsFormat::Junit, "target/nextest/default/junit.xml"),
            // nextest without fail-fast runs every test on every mutant, so each
            // mutant's log names all its killers (docs/adr/0004).
            mutants_command: "cargo mutants --workspace --no-shuffle --test-tool nextest --cargo-test-arg=--no-fail-fast \
                --output \"$SLIMFIT_OUT\""
                .into(),
            mutants_report: report(MutantsFormat::CargoMutants, "$SLIMFIT_OUT/mutants.out/outcomes.json"),
            mutants_file_flag: "--file".into(),
            ..base(&["**/*.rs"], &["tests/**", "**/tests/**", "benches/**", "**/benches/**", "examples/**"])
        },
        Preset::Go => Suite {
            test_command: "go test -json -coverprofile=\"$SLIMFIT_OUT/cover.out\" ./... > \"$SLIMFIT_OUT/test.json\"".into(),
            tests_report: report(TestsFormat::GoTestJson, "$SLIMFIT_OUT/test.json"),
            coverage_report: report(CoverageFormat::GoCover, "$SLIMFIT_OUT/cover.out"),
            mutants_command: "gremlins unleash --timeout-coefficient 10 --output \"$SLIMFIT_OUT/gremlins.json\"".into(),
            mutants_report: report(MutantsFormat::Gremlins, "$SLIMFIT_OUT/gremlins.json"),
            ..base(&["**/*.go"], &["**/*_test.go", "**/testdata/**"])
        },
        Preset::Swift => Suite {
            // XCTest writes xunit only with --parallel; one worker keeps per-test times.
            // .build/debug is a symlink, so find needs the trailing slash.
            test_command: "swift test --parallel --num-workers 1 --enable-code-coverage --xunit-output \"$SLIMFIT_OUT/xunit.xml\"; rc=$?; \
                bin=$(find .build/debug/ -maxdepth 1 -name '*.xctest' | head -1); \
                xcrun llvm-cov export -format=lcov -instr-profile=.build/debug/codecov/default.profdata \
                \"$bin/Contents/MacOS/$(basename \"$bin\" .xctest)\" > \"$SLIMFIT_OUT/lcov.info\"; exit $rc"
                .into(),
            tests_report: report(TestsFormat::Junit, "$SLIMFIT_OUT/xunit*.xml"),
            mutants_command: "[ -f muter.conf.yml ] || muter init; muter run --skip-update-check --format json --output \"$SLIMFIT_OUT/muter.json\"".into(),
            mutants_report: report(MutantsFormat::Muter, "$SLIMFIT_OUT/muter.json"),
            ..base(&["Sources/**/*.swift"], &["Tests/**"])
        },
        Preset::Kotlin => Suite {
            test_command: "./gradlew --quiet test koverXmlReport --continue".into(),
            tests_report: report(TestsFormat::Junit, "**/build/test-results/test/*.xml"),
            coverage_report: report(CoverageFormat::Jacoco, "**/build/reports/kover/report.xml"),
            mutants_command: "./gradlew --quiet pitest".into(),
            mutants_report: report(MutantsFormat::Pitest, "**/build/reports/pitest/mutations.xml"),
            ..base(&["**/src/main/**/*.kt", "**/src/main/**/*.java"], &["**/src/test/**", "**/src/androidTest/**"])
        },
        Preset::Typescript => Suite {
            test_command: "npx vitest run --coverage.enabled --coverage.reporter=lcov \
                --coverage.reportsDirectory=\"$SLIMFIT_OUT/coverage\" --reporter=junit --outputFile=\"$SLIMFIT_OUT/junit.xml\""
                .into(),
            tests_report: report(TestsFormat::Junit, "$SLIMFIT_OUT/junit.xml"),
            coverage_report: report(CoverageFormat::Lcov, "$SLIMFIT_OUT/coverage/lcov.info"),
            mutants_command: "npx stryker run --reporters json --disableBail".into(),
            mutants_report: report(MutantsFormat::Stryker, "reports/mutation/mutation.json"),
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

pub fn load(repo: &Path) -> Result<Config> {
    let path = repo.join(FILE);
    let text = std::fs::read_to_string(&path).with_context(|| format!("no {FILE}: run `slimfit init` first"))?;
    let cfg: Config = toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    if cfg.suites.is_empty() {
        bail!("{FILE} names no [[suite]]");
    }
    let mut names: Vec<&str> = cfg.suites.iter().map(|s| s.name.as_str()).collect();
    names.sort();
    if names.windows(2).any(|w| w[0] == w[1]) {
        bail!("{FILE}: two suites share a name");
    }
    Ok(cfg)
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

    #[test]
    fn overrides_replace_preset_fields() {
        let s: Config = toml::from_str(
            r#"
            [[suite]]
            name = "core"
            preset = "go"
            root = "core"
            test_command = "make test"
            "#,
        )
        .unwrap();
        let r = s.suites[0].resolve();
        assert_eq!(r.test_command, "make test");
        assert_eq!(r.tests_report.format, TestsFormat::GoTestJson);
        assert_eq!(r.root, "core");
    }
}
