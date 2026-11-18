use super::MutantsFormat;
use super::xml::{Node, walk};
use crate::model::{Mutant, MutantStatus};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;

/// The mutants, and every test the tool knew of when it records which tests
/// killed which mutant (`None` when it does not).
#[derive(Debug, Default)]
pub struct MutationRun {
    pub mutants: Vec<Mutant>,
    pub tests: Option<Vec<String>>,
}

pub fn read(format: MutantsFormat, texts: &[String]) -> Result<MutationRun> {
    let mut run = MutationRun::default();
    for t in texts {
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

fn line_of(v: &Value) -> u32 {
    v.as_u64().unwrap_or(0) as u32
}

fn cargo_mutants(text: &str, run: &mut MutationRun) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("cargo-mutants outcomes.json")?;
    for o in v["outcomes"].as_array().into_iter().flatten() {
        let m = &o["scenario"]["Mutant"];
        if m.is_null() {
            continue; // the baseline
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
            killed_by: None,
        });
    }
    Ok(())
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
        let r = read(MutantsFormat::Stryker, &[j.into()]).unwrap();
        let s: Vec<MutantStatus> = r.mutants.iter().map(|m| m.status).collect();
        assert_eq!(s, vec![MutantStatus::Killed, MutantStatus::Survived, MutantStatus::Unviable]);
        assert_eq!(r.mutants[0].killed_by, Some(vec!["adds".to_string()]));
        assert_eq!(r.tests, Some(vec!["adds".to_string(), "idle".to_string()]));
    }

    #[test]
    fn pit_names_match_junit_ids() {
        assert_eq!(
            pit_test_name("a.b.FooTest.[engine:junit-jupiter]/[class:a.b.FooTest]/[method:bar()]"),
            "a.b.FooTest::bar()"
        );
    }
}
