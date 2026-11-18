use super::MutantsFormat;
use super::xml::{Node, walk};
use crate::model::{Mutant, MutantStatus};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The mutants, and every test the tool knew of when it records which tests
/// killed which mutant (`None` when it does not).
#[derive(Debug, Default)]
pub struct MutationRun {
    pub mutants: Vec<Mutant>,
    pub tests: Option<Vec<String>>,
}

/// Reads each report, given with the path it was read from (cargo-mutants
/// keeps each mutant's test output in logs beside its report).
pub fn read(format: MutantsFormat, reports: &[(PathBuf, String)]) -> Result<MutationRun> {
    let mut run = MutationRun::default();
    for (path, t) in reports {
        match format {
            MutantsFormat::CargoMutants => cargo_mutants(t, path.parent().unwrap_or(Path::new(".")), &mut run)?,
            MutantsFormat::Stryker => stryker(t, &mut run)?,
            MutantsFormat::Pitest => pitest(t, &mut run)?,
            MutantsFormat::Gremlins => gremlins(t, &mut run)?,
            MutantsFormat::Muter => muter(t, &mut run)?,
        }
    }
    // One killed mutant whose killers are unknown makes every test that
    // seems to kill nothing look redundant: then claim nothing.
    if run.mutants.iter().any(|m| m.status == MutantStatus::Killed && m.killed_by.is_none()) {
        run.mutants.iter_mut().for_each(|m| m.killed_by = None);
        run.tests = None;
    } else if let Some(t) = run.tests.as_mut() {
        t.sort();
        t.dedup();
    }
    Ok(run)
}

fn line_of(v: &Value) -> u32 {
    v.as_u64().unwrap_or(0) as u32
}

/// `dir` holds `outcomes.json`, and the per-mutant logs its `log_path`s name.
/// The tests that killed a mutant are known only when its suite ran under
/// nextest with `--no-fail-fast`: then every test ran, and its log names
/// each one that failed.
fn cargo_mutants(text: &str, dir: &Path, run: &mut MutationRun) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("cargo-mutants outcomes.json")?;
    for o in v["outcomes"].as_array().into_iter().flatten() {
        let log = nextest_log(o, dir);
        if let Some(l) = &log {
            run.tests.get_or_insert_with(Vec::new).extend(l.started.iter().cloned());
        }
        let m = &o["scenario"]["Mutant"];
        if m.is_null() {
            // The baseline: when the unmutated tests fail, no mutant was
            // tested, and an empty report would read as a file with none.
            if o["scenario"] == "Baseline" && o["summary"] != "Success" {
                return Err(crate::fault::fault(
                    format!(
                        "cargo-mutants: the tests fail on the unmutated tree ({}), so no mutant was tested",
                        o["summary"].as_str().unwrap_or("?")
                    ),
                    format!(
                        "make the tests pass unmutated (its log {} says why they fail; `slimfit check` runs them), \
                         then run the mutants again",
                        o["log_path"].as_str().unwrap_or("")
                    ),
                ));
            }
            continue;
        }
        let summary = o["summary"].as_str().unwrap_or("");
        let status = match summary {
            "CaughtMutant" | "Timeout" => MutantStatus::Killed,
            "MissedMutant" => MutantStatus::Survived,
            _ => MutantStatus::Unviable,
        };
        let function = m["function"]["function_name"].as_str().unwrap_or("");
        let description = format!(
            "replace {} with {}",
            if function.is_empty() { m["genre"].as_str().unwrap_or("code") } else { function },
            m["replacement"].as_str().unwrap_or("?")
        );
        run.mutants.push(Mutant {
            file: m["file"].as_str().unwrap_or("").into(),
            line: line_of(&m["span"]["start"]["line"]),
            // name is "src/lib.rs:2:5: replace add -> i64 with 0"
            description: m["name"]
                .as_str()
                .and_then(|n| n.splitn(4, ':').nth(3))
                .map(|d| d.trim().to_string())
                .unwrap_or(description),
            status,
            // A timed-out run was stopped: the tests still running hung on the mutant.
            killed_by: log.map(|l| {
                let hung = (summary == "Timeout").then(|| l.started.difference(&l.finished));
                l.failed
                    .iter()
                    .chain(hung.into_iter().flatten())
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            }),
        });
    }
    Ok(())
}

/// What nextest's output in one outcome's log says, by JUnit id
/// (`<binary-id>::<test>`).
struct NextestLog {
    started: BTreeSet<String>,
    finished: BTreeSet<String>,
    failed: BTreeSet<String>,
}

/// The log of an outcome whose test phase ran nextest with `--no-fail-fast`.
fn nextest_log(o: &Value, dir: &Path) -> Option<NextestLog> {
    let complete = o["phase_results"].as_array()?.iter().any(|p| {
        let argv: Vec<&str> = p["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        p["phase"] == "Test" && argv.contains(&"nextest") && argv.contains(&"--no-fail-fast")
    });
    if !complete {
        return None;
    }
    let text = std::fs::read_to_string(dir.join(o["log_path"].as_str()?)).ok()?;
    let mut log = NextestLog { started: BTreeSet::new(), finished: BTreeSet::new(), failed: BTreeSet::new() };
    for (status, test) in text.lines().filter_map(nextest_line) {
        match status {
            "START" => {
                log.started.insert(test);
            }
            "SLOW" | "TERMINATING" | "SKIP" => {}
            s if s.starts_with("TRY ") => {} // a retry: its last attempt decides
            s => {
                if !matches!(s, "PASS" | "LEAK" | "FLAKY") {
                    log.failed.insert(test.clone());
                }
                log.finished.insert(test);
            }
        }
    }
    Some(log)
}

/// `   FAIL [   0.010s] (2/4) calc tests::adds` → `("FAIL", "calc::tests::adds")`.
fn nextest_line(line: &str) -> Option<(&str, String)> {
    let (status, rest) = line.trim_start().split_once(" [")?;
    if status.is_empty() || !status.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || " -".contains(c)) {
        return None;
    }
    let rest = rest.split_once("] ")?.1.trim_start();
    let rest = match rest.strip_prefix('(') {
        Some(r) => r.split_once(") ")?.1,
        None => rest,
    };
    let (binary, test) = rest.trim().split_once(' ')?;
    Some((status, format!("{binary}::{test}")))
}

/// https://github.com/stryker-mutator/mutation-testing-elements schema.
fn stryker(text: &str, run: &mut MutationRun) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("stryker mutation report")?;
    let mut names: HashMap<String, String> = HashMap::new();
    for file in v["testFiles"].as_object().into_iter().flat_map(|o| o.values()) {
        for t in file["tests"].as_array().into_iter().flatten() {
            let id = t["id"].as_str().unwrap_or("").to_string();
            names.insert(id, t["name"].as_str().unwrap_or("").to_string());
        }
    }
    let per_test = !names.is_empty();
    for (path, file) in v["files"].as_object().into_iter().flatten() {
        for m in file["mutants"].as_array().into_iter().flatten() {
            let status = match m["status"].as_str().unwrap_or("") {
                "Killed" | "Timeout" => MutantStatus::Killed,
                "Survived" | "NoCoverage" => MutantStatus::Survived,
                _ => MutantStatus::Unviable,
            };
            let killed_by = per_test.then(|| {
                m["killedBy"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| id.as_str())
                    .map(|id| names.get(id).cloned().unwrap_or_else(|| id.to_string()))
                    .collect()
            });
            run.mutants.push(Mutant {
                file: path.clone(),
                line: line_of(&m["location"]["start"]["line"]),
                description: format!(
                    "{}: {}",
                    m["mutatorName"].as_str().unwrap_or("?"),
                    m["replacement"].as_str().unwrap_or("").lines().next().unwrap_or("")
                ),
                status,
                killed_by,
            });
        }
    }
    if per_test {
        let mut all: Vec<String> = names.into_values().collect();
        all.sort();
        run.tests.get_or_insert_with(Vec::new).extend(all);
    }
    Ok(())
}

/// PIT records killers as `killingTest` (the first) or `killingTests`
/// (all, `|`-separated, with fullMutationMatrix).
fn pitest(text: &str, run: &mut MutationRun) -> Result<()> {
    #[derive(Default)]
    struct Cur {
        status: String,
        fields: HashMap<String, String>,
    }
    let mut cur: Option<Cur> = None;
    let mut field = String::new();
    let mut killers_known = false;
    walk(text, |node| match node {
        Node::Open { name, attrs } if name == "mutation" => {
            cur = Some(Cur { status: attrs.get("status").cloned().unwrap_or_default(), ..Default::default() });
        }
        Node::Open { name, .. } => field = name,
        Node::Text(t) => {
            if let Some(c) = cur.as_mut() {
                c.fields.entry(field.clone()).or_default().push_str(&t);
            }
        }
        Node::Close(name) if name == "mutation" => {
            let Some(c) = cur.take() else { return };
            let f = |k: &str| c.fields.get(k).cloned().unwrap_or_default();
            let status = match c.status.as_str() {
                "KILLED" | "TIMED_OUT" | "MEMORY_ERROR" => MutantStatus::Killed,
                "SURVIVED" | "NO_COVERAGE" => MutantStatus::Survived,
                _ => MutantStatus::Unviable,
            };
            let class = f("mutatedClass");
            let package = class.rsplit_once('.').map(|(p, _)| p.replace('.', "/")).unwrap_or_default();
            let file = if package.is_empty() { f("sourceFile") } else { format!("{package}/{}", f("sourceFile")) };
            let killers = if c.fields.contains_key("killingTests") { f("killingTests") } else { f("killingTest") };
            if c.fields.contains_key("killingTests") {
                killers_known = true;
            }
            run.mutants.push(Mutant {
                file,
                line: f("lineNumber").trim().parse().unwrap_or(0),
                description: format!("{}: {}", f("mutatedMethod"), f("description")),
                status,
                killed_by: Some(
                    killers.split('|').map(str::trim).filter(|s| !s.is_empty()).map(pit_test_name).collect(),
                ),
            });
        }
        Node::Close(_) => field.clear(),
    })?;
    // Without the full matrix PIT names only the first killer: not enough to
    // tell a redundant test, so drop the partial data.
    if !killers_known {
        for m in &mut run.mutants {
            m.killed_by = None;
        }
    }
    Ok(())
}

/// `a.b.FooTest.[engine:junit-jupiter]/[class:a.b.FooTest]/[method:bar()]`
/// → `a.b.FooTest::bar()`, the JUnit report's id for the same test.
fn pit_test_name(raw: &str) -> String {
    let class = raw.split(".[").next().unwrap_or(raw);
    match raw.rsplit_once("[method:") {
        Some((_, m)) => format!("{class}::{}", m.trim_end_matches(']')),
        None => raw.to_string(),
    }
}

fn gremlins(text: &str, run: &mut MutationRun) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("gremlins report")?;
    for f in v["files"].as_array().into_iter().flatten() {
        let file = f["file_name"].as_str().unwrap_or("");
        for m in f["mutations"].as_array().into_iter().flatten() {
            let status = match m["status"].as_str().unwrap_or("") {
                "KILLED" | "TIMED OUT" => MutantStatus::Killed,
                "LIVED" | "NOT COVERED" => MutantStatus::Survived,
                _ => MutantStatus::Unviable,
            };
            run.mutants.push(Mutant {
                file: file.into(),
                line: line_of(&m["line"]),
                description: m["type"].as_str().unwrap_or("?").to_string(),
                status,
                killed_by: None,
            });
        }
    }
    Ok(())
}

fn muter(text: &str, run: &mut MutationRun) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("muter report")?;
    let ops = v["fileReports"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|f| f["appliedOperators"].as_array().into_iter().flatten());
    for o in ops {
        let status = match o["testSuiteOutcome"].as_str().unwrap_or("") {
            "failed" | "runtimeError" | "timedOut" => MutantStatus::Killed,
            "passed" | "noCoverage" => MutantStatus::Survived,
            _ => MutantStatus::Unviable,
        };
        let point = &o["mutationPoint"];
        // muter mutates a copy of the package at `<package>_mutated/`
        let path = point["filePath"].as_str().unwrap_or("");
        let path = path.split_once("_mutated/").map(|(_, rest)| rest).unwrap_or(path);
        run.mutants.push(Mutant {
            file: path.into(),
            line: line_of(&point["position"]["line"]),
            description: point["mutationOperatorId"].as_str().unwrap_or("?").to_string(),
            status,
            killed_by: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn stryker_maps_status_and_killers() {
        let j = r#"{"files":{"src/a.ts":{"mutants":[
            {"id":"1","mutatorName":"EqualityOperator","replacement":"a < b","location":{"start":{"line":3,"column":1}},"status":"Killed","killedBy":["t1"]},
            {"id":"2","mutatorName":"BlockStatement","replacement":"{}","location":{"start":{"line":5,"column":1}},"status":"NoCoverage"},
            {"id":"3","mutatorName":"X","replacement":"","location":{"start":{"line":6,"column":1}},"status":"CompileError"}]}},
          "testFiles":{"src/a.test.ts":{"tests":[{"id":"t1","name":"adds"},{"id":"t2","name":"idle"}]}}}"#;
        let r = read(MutantsFormat::Stryker, &[(PathBuf::new(), j.into())]).unwrap();
        let s: Vec<MutantStatus> = r.mutants.iter().map(|m| m.status).collect();
        assert_eq!(s, vec![MutantStatus::Killed, MutantStatus::Survived, MutantStatus::Unviable]);
        assert_eq!(r.mutants[0].killed_by, Some(vec!["adds".to_string()]));
        assert_eq!(r.tests, Some(vec!["adds".to_string(), "idle".to_string()]));
    }

    #[test]
    fn nextest_lines_name_tests_by_junit_id() {
        assert_eq!(
            nextest_line("        FAIL [   0.010s] (2/4) calc tests::adds"),
            Some(("FAIL", "calc::tests::adds".into()))
        );
        assert_eq!(
            nextest_line("  START [         ] (1/4) calc::it it_works"),
            Some(("START", "calc::it::it_works".into()))
        );
        assert_eq!(nextest_line("   TRY 2 FAIL [   1.000s] a t"), Some(("TRY 2 FAIL", "a::t".into())));
        assert_eq!(nextest_line("    thread 'tests::adds' panicked at src/lib.rs:21:9:"), None);
        assert_eq!(nextest_line("     Summary [   0.611s] 4 tests run: 1 passed, 3 failed"), None);
    }

    #[test]
    fn a_timed_out_mutant_is_killed_by_the_tests_still_running_and_cargo_test_records_none() {
        let dir = tempfile::tempdir().unwrap();
        let log = "  START [ ] (1/2) c t::quick\n  START [ ] (2/2) c t::hangs\nTRY 1 FAIL [ 0.01s] (1/2) c t::quick\n   PASS [ 0.01s] (1/2) c t::quick\n   SLOW [> 60s] (2/2) c t::hangs\n";
        std::fs::write(dir.path().join("hang.log"), log).unwrap();
        let outcome = |summary: &str, phase: &str, tool: &str| {
            format!(
                r#"{{"scenario":{{"Mutant":{{"file":"src/a.rs","name":"src/a.rs:1:1: replace f with ()"}}}},"summary":"{summary}","log_path":"hang.log",
                    "phase_results":[{{"phase":"{phase}","argv":["cargo","{tool}","run","--no-fail-fast"]}}]}}"#
            )
        };
        let report = |tool: &str| {
            let text = format!(
                r#"{{"outcomes":[{},{}]}}"#,
                outcome("Timeout", "Test", tool),
                outcome("Unviable", "Build", tool)
            );
            read(MutantsFormat::CargoMutants, &[(dir.path().join("outcomes.json"), text)]).unwrap()
        };
        let r = report("nextest");
        assert_eq!(r.mutants[0].killed_by, Some(vec!["c::t::hangs".to_string()]));
        assert_eq!(r.mutants[1].status, MutantStatus::Unviable);
        assert_eq!(r.tests, Some(vec!["c::t::hangs".to_string(), "c::t::quick".to_string()]));
        let r = report("test");
        assert!(r.tests.is_none() && r.mutants[0].killed_by.is_none());
        let failed = r#"{"outcomes":[{"scenario":"Baseline","summary":"Failure","log_path":"log/baseline.log"}]}"#;
        let e = read(MutantsFormat::CargoMutants, &[(dir.path().join("outcomes.json"), failed.into())]).unwrap_err();
        let f = &crate::fault::of(e)[0];
        assert!(f.fault.contains("fail on the unmutated tree (Failure)"), "{}", f.fault);
        assert!(f.fix.starts_with("make the tests pass unmutated (its log log/baseline.log"), "{}", f.fix);
    }

    #[test]
    fn pit_names_match_junit_ids() {
        assert_eq!(
            pit_test_name("a.b.FooTest.[engine:junit-jupiter]/[class:a.b.FooTest]/[method:bar()]"),
            "a.b.FooTest::bar()"
        );
    }
}
