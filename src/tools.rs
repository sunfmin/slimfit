//! What slimfit and the tools it drives agree on: where a tool writes a
//! report when it picks the place itself, and how slimfit tells a command
//! where its scratch directory is. The stand-ins in `examples/standin/`
//! include this file, so they write where the presets read.

/// The environment variable that names a suite's scratch directory to its
/// commands; report paths may start with `$SLIMFIT_OUT`.
pub const OUT_ENV: &str = "SLIMFIT_OUT";

/// The environment variables that override `[check] before_make` and
/// `before_remove`; those commands are given the tree's path, its rev and
/// what it is for in the three after them.
pub const BEFORE_MAKE_ENV: &str = "SLIMFIT_BEFORE_MAKE";
pub const BEFORE_REMOVE_ENV: &str = "SLIMFIT_BEFORE_REMOVE";
pub const TREE_ENV: &str = "SLIMFIT_TREE";
pub const REV_ENV: &str = "SLIMFIT_REV";
pub const DESCRIPTION_ENV: &str = "SLIMFIT_DESCRIPTION";

/// The environment variable that gives a suite's commands its `test_args`.
pub const TEST_ARGS_ENV: &str = "SLIMFIT_TEST_ARGS";
/// nextest's store: a directory per profile, holding its JUnit report.
pub const NEXTEST_STORE: &str = "target/nextest";
/// The JUnit file name slimfit configures nextest with.
pub const NEXTEST_JUNIT: &str = "junit.xml";
/// cargo-mutants' report, under its `--output` directory.
pub const CARGO_MUTANTS_OUTCOMES: &str = "mutants.out/outcomes.json";
/// Gradle's directory of JUnit XML for the `test` task, under a project.
pub const GRADLE_TEST_RESULTS: &str = "build/test-results/test";
/// Kover's XML report (`koverXmlReport`), under a project.
pub const KOVER_XML: &str = "build/reports/kover/report.xml";
/// PIT's XML report (`pitest`), under a project.
pub const PITEST_XML: &str = "build/reports/pitest/mutations.xml";
/// Stryker's JSON report, under the project root.
pub const STRYKER_JSON: &str = "reports/mutation/mutation.json";
/// SwiftPM's debug build directory (a symlink) and its coverage profile.
pub const SWIFT_DEBUG: &str = ".build/debug";
pub const SWIFT_PROFDATA: &str = ".build/debug/codecov/default.profdata";
/// The mutmut the Python preset drives, and where its driver writes the
/// outcomes, under `$SLIMFIT_OUT`.
pub const MUTMUT: &str = "mutmut==3.8.1";
pub const MUTMUT_OUTCOMES: &str = "mutmut.json";
/// The Rust, Go and Python presets' report of what each test ran, under
/// `$SLIMFIT_OUT`.
pub const REACH: &str = "reach.txt";
/// The directory where a test command records each test's CPU time, under
/// `$SLIMFIT_OUT`, made before it runs (ADR 0010): a file per test process, its first line the test's id as the
/// tests report gives it, then what the shell's `times` printed after the
/// test exited (its second line: the CPU time of the shell's children).
pub const TEST_CPU: &str = "test-cpu";
