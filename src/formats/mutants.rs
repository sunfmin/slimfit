use super::MutantsFormat;
use super::xml::{Node, walk};
use crate::model::{Candidate, Mutant, MutantStatus};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;

/// Reads each report into its mutants.
pub fn read(format: MutantsFormat, reports: &[String]) -> Result<Vec<Mutant>> {
    let mut run = Vec::new();
    for t in reports {
        match format {
            MutantsFormat::CargoMutants => cargo_mutants(t, &mut run)?,
            MutantsFormat::Stryker => stryker(t, &mut run)?,
            MutantsFormat::Pitest => pitest(t, &mut run)?,
            MutantsFormat::Gremlins => gremlins(t, &mut run)?,
            MutantsFormat::Muter => muter(t, &mut run)?,
        }
    }
    Ok(run)
}

/// The mutants `cargo mutants --list --json` lists, paths as it gives them
/// (relative to where it ran).
pub fn list(text: &str) -> Result<Vec<Candidate>> {
    let v: Value = serde_json::from_str(text).context("cargo mutants --list --json")?;
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let listed = v.as_array().context("cargo mutants --list --json is not a list")?;
    Ok(listed
        .iter()
        .map(|m| Candidate {
            name: s(&m["name"]),
            file: s(&m["file"]),
            line: line_of(&m["span"]["start"]["line"]),
            function: s(&m["function"]["function_name"]),
            replacement: s(&m["replacement"]),
            end: line_of(&m["span"]["end"]["line"]),
            field: m["genre"] == "StructField",
        })
        .collect())
}

fn line_of(v: &Value) -> u32 {
    v.as_u64().unwrap_or(0) as u32
}

/// cargo-mutants' `outcomes.json`.
fn cargo_mutants(text: &str, run: &mut Vec<Mutant>) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("cargo-mutants outcomes.json")?;
    for o in v["outcomes"].as_array().into_iter().flatten() {
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
        let status = match o["summary"].as_str().unwrap_or("") {
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
        run.push(Mutant {
            file: m["file"].as_str().unwrap_or("").into(),
            line: line_of(&m["span"]["start"]["line"]),
            // name is "src/lib.rs:2:5: replace add -> i64 with 0"
            description: m["name"]
                .as_str()
                .and_then(|n| n.splitn(4, ':').nth(3))
                .map(|d| d.trim().to_string())
                .unwrap_or(description),
            status,
        });
    }
    Ok(())
}

/// https://github.com/stryker-mutator/mutation-testing-elements schema.
fn stryker(text: &str, run: &mut Vec<Mutant>) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("stryker mutation report")?;
    for (path, file) in v["files"].as_object().into_iter().flatten() {
        for m in file["mutants"].as_array().into_iter().flatten() {
            let status = match m["status"].as_str().unwrap_or("") {
                "Killed" | "Timeout" => MutantStatus::Killed,
                "Survived" | "NoCoverage" => MutantStatus::Survived,
                _ => MutantStatus::Unviable,
            };
            run.push(Mutant {
                file: path.clone(),
                line: line_of(&m["location"]["start"]["line"]),
                description: format!(
                    "{}: {}",
                    m["mutatorName"].as_str().unwrap_or("?"),
                    m["replacement"].as_str().unwrap_or("").lines().next().unwrap_or("")
                ),
                status,
            });
        }
    }
    Ok(())
}

/// PIT's `mutations.xml`.
fn pitest(text: &str, run: &mut Vec<Mutant>) -> Result<()> {
    #[derive(Default)]
    struct Cur {
        status: String,
        fields: HashMap<String, String>,
    }
    let mut cur: Option<Cur> = None;
    let mut field = String::new();
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
            run.push(Mutant {
                file,
                line: f("lineNumber").trim().parse().unwrap_or(0),
                description: format!("{}: {}", f("mutatedMethod"), f("description")),
                status,
            });
        }
        Node::Close(_) => field.clear(),
    })?;
    Ok(())
}

fn gremlins(text: &str, run: &mut Vec<Mutant>) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("gremlins report")?;
    for f in v["files"].as_array().into_iter().flatten() {
        let file = f["file_name"].as_str().unwrap_or("");
        for m in f["mutations"].as_array().into_iter().flatten() {
            let status = match m["status"].as_str().unwrap_or("") {
                "KILLED" | "TIMED OUT" => MutantStatus::Killed,
                "LIVED" | "NOT COVERED" => MutantStatus::Survived,
                _ => MutantStatus::Unviable,
            };
            run.push(Mutant {
                file: file.into(),
                line: line_of(&m["line"]),
                description: m["type"].as_str().unwrap_or("?").to_string(),
                status,
            });
        }
    }
    Ok(())
}

fn muter(text: &str, run: &mut Vec<Mutant>) -> Result<()> {
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
        run.push(Mutant {
            file: path.into(),
            line: line_of(&point["position"]["line"]),
            description: point["mutationOperatorId"].as_str().unwrap_or("?").to_string(),
            status,
        });
    }
    Ok(())
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn stryker_maps_status() {
        let j = r#"{"files":{"src/a.ts":{"mutants":[
            {"id":"1","mutatorName":"EqualityOperator","replacement":"a < b","location":{"start":{"line":3,"column":1}},"status":"Killed","killedBy":["t1"]},
            {"id":"2","mutatorName":"BlockStatement","replacement":"{}","location":{"start":{"line":5,"column":1}},"status":"NoCoverage"},
            {"id":"3","mutatorName":"X","replacement":"","location":{"start":{"line":6,"column":1}},"status":"CompileError"}]}}}"#;
        let r = read(MutantsFormat::Stryker, &[j.into()]).unwrap();
        let s: Vec<MutantStatus> = r.iter().map(|m| m.status).collect();
        assert_eq!(s, vec![MutantStatus::Killed, MutantStatus::Survived, MutantStatus::Unviable]);
        assert_eq!((r[0].line, r[0].description.as_str()), (3, "EqualityOperator: a < b"));
    }

    /// PIT escapes `<init>` and quotes in element text: they read back as written.
    #[test]
    fn pitest_unescapes_references_in_text() {
        let x = "<mutations><mutation status='KILLED'><sourceFile>A.kt</sourceFile>\
            <mutatedClass>p.A</mutatedClass><mutatedMethod>&lt;init&gt;</mutatedMethod><lineNumber>2</lineNumber>\
            <description>&quot;a&quot; &amp; &apos;b&apos; &#65;</description></mutation></mutations>";
        let r = read(MutantsFormat::Pitest, &[x.into()]).unwrap();
        assert_eq!((r[0].file.as_str(), r[0].line), ("p/A.kt", 2));
        assert_eq!(r[0].description, "<init>: \"a\" & 'b' A");
    }

    /// A timed-out mutant is killed; when the unmutated tests fail, no
    /// mutant was tested, and the fault names the baseline's log.
    #[test]
    fn cargo_mutants_reads_outcomes_and_refuses_a_failing_baseline() {
        let outcome = |summary: &str| {
            format!(
                r#"{{"scenario":{{"Mutant":{{"file":"src/a.rs","name":"src/a.rs:1:1: replace f with ()"}}}},"summary":"{summary}"}}"#
            )
        };
        let text = format!(r#"{{"outcomes":[{},{}]}}"#, outcome("Timeout"), outcome("Unviable"));
        let r = read(MutantsFormat::CargoMutants, &[text]).unwrap();
        let got: Vec<_> = r.iter().map(|m| (m.status, m.description.as_str())).collect();
        assert_eq!(got, [(MutantStatus::Killed, "replace f with ()"), (MutantStatus::Unviable, "replace f with ()")]);
        let failed = r#"{"outcomes":[{"scenario":"Baseline","summary":"Failure","log_path":"log/baseline.log"}]}"#;
        let e = read(MutantsFormat::CargoMutants, &[failed.into()]).unwrap_err();
        let f = &crate::fault::of(e)[0];
        assert!(f.fault.contains("fail on the unmutated tree (Failure)"), "{}", f.fault);
        assert!(f.fix.starts_with("make the tests pass unmutated (its log log/baseline.log"), "{}", f.fix);
    }
}
