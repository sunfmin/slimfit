//! Real reports, captured from real tool runs over the sample projects in
//! `testdata/<language>/`, read end to end into tasks. Every sample holds
//! the same five situations: a tested `add`, an uncovered `unused`, an
//! `is_adult` whose boundary mutant survives, a redundant copy of the `add`
//! test, and a slow test.

use crate::config::{CheckConfig, Config, Preset, preset};
use crate::formats::{self, CoverageFormat, MutantsFormat, TestsFormat};
use crate::landing::Tree;
use crate::measure::{Timing, reduce};
use crate::model::{Measurement, MutantStatus, SuiteMeasurement};
use crate::ratchet::{Baseline, check, ratchet};
use crate::tasks::{TaskKind, of_measurement};
use serde_json::{Value, json};
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
    // The reports' `/REPO` is the sample's directory, where its files are read.
    let tree = Tree::at(&dir, files);
    let st = tree.suite(&Config::one(lang, p), lang).unwrap();
    let here = |texts: Vec<String>| -> Vec<String> {
        texts.into_iter().map(|t| t.replace("/REPO", &dir.to_string_lossy())).collect()
    };
    let t = formats::read_tests(tests.0, &read(lang, tests.1)).unwrap();
    let c = formats::read_coverage(coverage.0, &here(coverage.1)).unwrap();
    let mut m = formats::read_mutants(mutants.0, &here(read(lang, mutants.1))).unwrap();
    for x in &mut m {
        x.file = st.resolver.resolve(&x.file).unwrap_or_else(|| x.file.clone());
    }
    // The Rust, Go and Python samples' test runs also record what each test ran.
    let r = preset(p).reach_report;
    let reach = (!r.path.is_empty()).then(|| formats::read_reach(r.format, &here(read(lang, &["reach.txt"]))).unwrap());
    // The tests' CPU time as their wall time: the sample's reports hold no other.
    let tree_cpu = t.iter().map(|t| t.seconds).sum();
    reduce(st, t, c, reach, Some(m.into()), Timing { command_seconds: 0.0, tree_cpu, test_cpu: Default::default() })
}

/// Each redundant-test candidate with its class (slimfit#88), task by task.
fn classes(s: &SuiteMeasurement) -> Vec<(String, Value)> {
    let tasks = of_measurement(s, &[]);
    let candidates = tasks
        .iter()
        .filter(|t| t.kind == TaskKind::RedundantTest)
        .flat_map(|t| t.evidence["candidates"].as_array().unwrap().clone());
    candidates.map(|c| (c["test"].as_str().unwrap().into(), c["class"].clone())).collect()
}

/// What every sample must show, whatever the language.
fn assert_the_five_situations(s: &SuiteMeasurement, source: &str, adult_line: u32) {
    assert_eq!(s.metrics.failures, 0, "{}: no failures", s.suite);
    assert!(s.metrics.tests >= 4, "{}: four tests, got {:?}", s.suite, s.tests);
    let tasks = of_measurement(s, &[]);
    let kinds: Vec<TaskKind> = tasks.iter().map(|t| t.kind).collect();

    let unused = s.uncovered.iter().find(|sp| sp.file == source && sp.lines >= 3);
    assert!(unused.is_some(), "{}: `unused` is uncovered: {:?}", s.suite, s.uncovered);
    // Its fix asserts what the lines decide, or deletes them; running them is none (slimfit#67).
    let steps = &tasks.iter().find(|t| t.kind == TaskKind::Uncovered).expect("an uncovered task").steps;
    assert!(steps.contains("asserts what these lines decide") && steps.contains("delete it"), "{steps}");
    assert!(steps.contains("A test that only runs the lines, asserting nothing they decide, is no fix"), "{steps}");

    let slow = s.tests.iter().find(|t| t.id.to_lowercase().contains("slow")).expect("slow test");
    assert!(slow.cpu >= 0.5, "{}: slow test timed: {slow:?}", s.suite);
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
    // Which tests ran each function: `add` the three that call it, `unused`
    // none, `is_adult` one; the tests' own functions are not listed.
    let reach = s.reach.as_ref().expect("the Rust sample records reach");
    let ran = |line: u32| -> Vec<String> {
        reach.functions["src/lib.rs"][&line].iter().map(|i| reach.tests[*i as usize].id()).collect::<Vec<_>>()
    };
    assert_eq!(reach.functions["src/lib.rs"].keys().copied().collect::<Vec<_>>(), [1, 5, 11]);
    let mut add = ran(1);
    add.sort();
    assert_eq!(add, ["calc::tests::adds", "calc::tests::adds_again", "calc::tests::slow"]);
    assert_eq!((ran(5), ran(11)), (vec![], vec!["calc::tests::adult".to_string()]));
    // And which source lines each ran, its own test code left out; each
    // test's own function is found, from its `fn` line.
    let lines = reach.lines.as_ref().expect("the Rust sample records lines");
    let of = |id: &str| reach.tests.iter().position(|t| t.id() == id).unwrap();
    assert_eq!(lines[of("calc::tests::adds")], [("src/lib.rs".to_string(), vec![1, 2, 3])].into());
    assert_eq!(lines[of("calc::tests::adult")], [("src/lib.rs".to_string(), vec![11, 12, 13])].into());
    let source = reach.tests[of("calc::tests::adds")].source.as_ref().unwrap();
    assert_eq!((source.file.as_str(), source.line), ("src/lib.rs", 20));
    // The copy of `adds` asserts what `adds` does: delete it; `slow` asserts another sum: read.
    let want = [("calc::tests::slow", "read"), ("calc::tests::adds_again", "asserts-contained")];
    assert_eq!(classes(&s), want.map(|(t, c)| (t.to_string(), json!(c))));
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
            let v = check(&base, m, &cfg, None);
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
    // Each test's own run gives its lines, so the copy of the add test is
    // proposed, in the test file its func is in.
    let tasks = of_measurement(&s, &[]);
    let redundant: Vec<_> = tasks.iter().filter(|t| t.kind == TaskKind::RedundantTest).collect();
    assert!(!redundant.is_empty(), "{tasks:#?}");
    assert!(redundant.iter().all(|t| t.files == ["calc_test.go"]), "{redundant:#?}");
    let classes_ = [("example.com/calc::TestSlow", "read"), ("example.com/calc::TestAddsAgain", "asserts-contained")];
    assert_eq!(classes(&s), classes_.map(|(t, c)| (t.to_string(), json!(c))));
}

/// Python's test run records each test's lines (coverage.py's contexts),
/// so the copy of the add test is proposed, in the module its `def` is in;
/// mutmut's driver reports in cargo-mutants' shape.
#[test]
fn python_sample() {
    let s = measure(
        "python",
        Preset::Python,
        (TestsFormat::Junit, &["junit.xml"]),
        (CoverageFormat::Lcov, &["lcov.info"]),
        (MutantsFormat::CargoMutants, &["mutmut.json"]),
    );
    assert_the_five_situations(&s, "src/calc.py", 12);
    let tasks = of_measurement(&s, &[]);
    let redundant: Vec<_> = tasks.iter().filter(|t| t.kind == TaskKind::RedundantTest).collect();
    assert!(!redundant.is_empty(), "{tasks:#?}");
    assert!(redundant.iter().all(|t| t.files == ["tests/test_calc.py"]), "{redundant:#?}");
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

/// A workspace of two packages: `double` in `core` has no test of its own;
/// only `app`'s test reaches it. Under the preset's mutants command every
/// mutant runs the whole workspace's tests, so `app`'s test catches each of
/// `double`'s mutants.
#[test]
fn a_mutant_in_one_package_is_caught_by_another_packages_test() {
    let suite = preset(Preset::Rust);
    assert!(suite.mutants_command.contains("--test-workspace=true"), "{}", suite.mutants_command);
    let m = formats::read_mutants(MutantsFormat::CargoMutants, &read("rust-workspace", &["outcomes.json"])).unwrap();
    let double: Vec<_> = m.iter().filter(|x| x.file == "core/src/lib.rs" && x.line == 4).collect();
    assert_eq!(double.len(), 5, "{m:?}");
    assert!(double.iter().all(|x| x.status == MutantStatus::Killed), "{double:?}");
}
