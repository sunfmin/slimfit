//! Real reports, captured from real tool runs over the sample projects in
//! `testdata/<language>/`, read end to end into tasks. Every sample holds
//! the same five situations: a tested `add`, an uncovered `unused`, an
//! `is_adult` whose boundary mutant survives, a redundant copy of the `add`
//! test, and a slow test.

use crate::config::{CheckConfig, Preset, preset};
use crate::formats::{self, CoverageFormat, MutantsFormat, TestsFormat};
use crate::loc::Classifier;
use crate::measure::{Resolver, reduce};
use crate::model::{Measurement, MutantStatus, SuiteMeasurement};
use crate::ratchet::{Baseline, check, ratchet};
use crate::tasks::{TaskKind, of_measurement};
use std::path::{Path, PathBuf};

fn sample(lang: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata").join(lang)
}

fn files_under(dir: &Path, base: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !["reports", "node_modules", ".build", "build", "target", ".gradle", ".out"].contains(&name.as_str()) {
                files_under(&p, base, out);
            }
        } else {
            out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
        }
    }
}

fn read(lang: &str, names: &[&str]) -> Vec<String> {
    names.iter().map(|n| std::fs::read_to_string(sample(lang).join("reports").join(n)).unwrap()).collect()
}

fn measure(
    lang: &str,
    p: Preset,
    tests: (TestsFormat, &[&str]),
    coverage: (CoverageFormat, &[&str]),
    mutants: (MutantsFormat, &[&str]),
) -> SuiteMeasurement {
    measure_with_coverage(lang, p, tests, (coverage.0, read(lang, coverage.1)), mutants)
}

fn measure_with_coverage(
    lang: &str,
    p: Preset,
    tests: (TestsFormat, &[&str]),
    coverage: (CoverageFormat, Vec<String>),
    mutants: (MutantsFormat, &[&str]),
) -> SuiteMeasurement {
    let dir = sample(lang);
    let mut files = Vec::new();
    files_under(&dir, &dir, &mut files);
    let mut suite = preset(p);
    suite.name = lang.into();
    let totals = Classifier::new(&suite).unwrap().count(&files, |f| std::fs::read_to_string(dir.join(f)).ok());
    let resolver = Resolver::new(Path::new("/REPO"), &totals);
    let t = formats::read_tests(tests.0, &read(lang, tests.1)).unwrap();
    let c = formats::read_coverage(coverage.0, &coverage.1).unwrap();
    let mut m = formats::read_mutants(mutants.0, &read(lang, mutants.1)).unwrap();
    for x in &mut m.mutants {
        x.file = resolver.resolve(&x.file).unwrap_or_else(|| x.file.clone());
    }
    reduce(&suite, &totals, &resolver, t, c, Some(m), 0.0)
}

/// What every sample must show, whatever the language.
fn assert_the_five_situations(s: &SuiteMeasurement, source: &str, adult_line: u32) {
    assert_eq!(s.metrics.failures, 0, "{}: no failures", s.suite);
    assert!(s.metrics.tests >= 4, "{}: four tests, got {:?}", s.suite, s.tests);
    let tasks = of_measurement(s);
    let kinds: Vec<TaskKind> = tasks.iter().map(|t| t.kind).collect();

    let unused = s.uncovered.iter().find(|sp| sp.file == source && sp.lines >= 3);
    assert!(unused.is_some(), "{}: `unused` is uncovered: {:?}", s.suite, s.uncovered);
    assert!(kinds.contains(&TaskKind::Uncovered));

    let slow = s.tests.iter().find(|t| t.id.to_lowercase().contains("slow")).expect("slow test");
    assert!(slow.seconds >= 0.5, "{}: slow test timed: {slow:?}", s.suite);
    assert!(tasks.iter().any(|t| t.kind == TaskKind::SlowTest && t.summary.contains(&slow.id)));

    let adult_survivor =
        s.mutants.iter().any(|m| m.file == source && m.line == adult_line && m.status == MutantStatus::Survived);
    assert!(adult_survivor, "{}: the is_adult boundary mutant survives: {:?}", s.suite, s.mutants);
    assert!(kinds.contains(&TaskKind::SurvivingMutant));
    assert!(s.metrics.prod_lines > 0 && s.metrics.test_lines > 0);
}

#[test]
fn rust_sample() {
    let s = measure(
        "rust",
        Preset::Rust,
        (TestsFormat::Junit, &["junit.xml"]),
        (CoverageFormat::Lcov, &["lcov.info"]),
        (MutantsFormat::CargoMutants, &["outcomes.json"]),
    );
    assert_the_five_situations(&s, "src/lib.rs", 12);
    // the inline test module is test code, not uncovered prod code
    assert!(s.uncovered.iter().all(|sp| sp.end < 16), "{:?}", s.uncovered);
    assert_eq!(s.uncovered[0].function.as_deref(), Some("unused"));
}

/// Coverage that depends on timing reaches a line in one run and misses it
/// in the next, with no code changed. Neither run may fail `check` against
/// a baseline taken from the other, nor be offered to `ratchet` as a gain.
#[test]
fn a_line_that_flaps_neither_fails_check_nor_ratchets() {
    let run = |lcov: String| Measurement {
        tree: "t".into(),
        with_mutants: false,
        suites: vec![measure_with_coverage(
            "rust",
            Preset::Rust,
            (TestsFormat::Junit, &["junit.xml"]),
            (CoverageFormat::Lcov, vec![lcov]),
            (MutantsFormat::CargoMutants, &[]),
        )],
    };
    let real = read("rust", &["lcov.info"]).remove(0);
    let reached = run(real.clone());
    let missed = run(real.replacen("DA:12,1", "DA:12,0", 1));
    let uncovered = |m: &Measurement| m.suites[0].metrics.uncovered_lines;
    assert_eq!(uncovered(&missed), uncovered(&reached) + 1);

    let cfg = CheckConfig::default();
    for (first, second) in [(&reached, &missed), (&missed, &reached)] {
        let mut base = Baseline::default();
        base = ratchet(&base, first, &cfg, &[], None).unwrap();
        for m in [second, first, second] {
            let v = check(&base, m, &cfg);
            assert!(v.ok && v.improved.is_empty(), "{v:?}");
            base = ratchet(&base, m, &cfg, &[], None).unwrap();
            assert_eq!(base.suites["rust"].uncovered_lines, uncovered(first));
        }
    }
}

#[test]
fn go_sample() {
    let s = measure(
        "go",
        Preset::Go,
        (TestsFormat::GoTestJson, &["test.json"]),
        (CoverageFormat::GoCover, &["cover.out"]),
        (MutantsFormat::Gremlins, &["gremlins.json"]),
    );
    assert_the_five_situations(&s, "calc.go", 14);
}

#[test]
fn typescript_sample() {
    let s = measure(
        "typescript",
        Preset::Typescript,
        (TestsFormat::Junit, &["junit.xml"]),
        (CoverageFormat::Lcov, &["lcov.info"]),
        (MutantsFormat::Stryker, &["mutation.json"]),
    );
    assert_the_five_situations(&s, "src/calc.ts", 12);
    // Stryker with --disableBail records every killer: the cheapest of the
    // three identical `add` tests stays, the other two are redundant.
    let redundant: Vec<String> = of_measurement(&s)
        .into_iter()
        .filter(|t| t.kind == TaskKind::RedundantTest)
        .map(|t| t.evidence["test"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(redundant.len(), 2, "{redundant:?}");
    assert!(redundant.contains(&"calc > slow".to_string()), "{redundant:?}");
    assert!(!redundant.contains(&"calc > isAdult".to_string()));
}

#[test]
fn kotlin_sample() {
    let s = measure(
        "kotlin",
        Preset::Kotlin,
        (TestsFormat::Junit, &["junit-com.example.CalcTest.xml"]),
        (CoverageFormat::Jacoco, &["kover.xml"]),
        (MutantsFormat::Pitest, &["mutations.xml"]),
    );
    let source = "src/main/kotlin/com/example/Calc.kt";
    assert_the_five_situations(&s, source, 11);
    // PIT's full mutation matrix names every killer, in JUnit's own ids.
    let redundant: Vec<String> = of_measurement(&s)
        .into_iter()
        .filter(|t| t.kind == TaskKind::RedundantTest)
        .map(|t| t.evidence["test"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(redundant.len(), 2, "{redundant:?}");
    assert!(redundant.contains(&"com.example.CalcTest::slow()".to_string()), "{redundant:?}");
    assert!(s.tests.iter().any(|t| t.id == "com.example.CalcTest::slow()"));
}

#[test]
fn swift_sample() {
    // muter has no `>=`→`>` operator, so the Swift sample tests `isAdult(18)`:
    // its `>=`→`<=` mutant survives (`muter-survived.json`). `muter.json` is
    // the same run with the test at 30, where that mutant dies.
    let s = measure(
        "swift",
        Preset::Swift,
        (TestsFormat::Junit, &["xunit.xml", "xunit-swift-testing.xml"]),
        (CoverageFormat::Lcov, &["lcov.info"]),
        (MutantsFormat::Muter, &["muter-survived.json"]),
    );
    assert_the_five_situations(&s, "Sources/Calc/Calc.swift", 11);
    let killed = measure(
        "swift",
        Preset::Swift,
        (TestsFormat::Junit, &["xunit.xml"]),
        (CoverageFormat::Lcov, &["lcov.info"]),
        (MutantsFormat::Muter, &["muter.json"]),
    );
    assert_eq!(killed.mutants[0].status, MutantStatus::Killed);
    assert_eq!(killed.mutants[0].file, "Sources/Calc/Calc.swift");
}
