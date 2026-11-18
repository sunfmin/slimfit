//! Turning a measurement into work: one task at a time, cheapest and
//! safest kinds first, each with its evidence and the command that accepts it.

use crate::config::Config;
use crate::git;
use crate::loc::{Classifier, Kind};
use crate::model::{Measurement, MutantStatus, SuiteMeasurement};
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    FailingTest,
    Uncovered,
    RedundantTest,
    SlowTest,
    SurvivingMutant,
    Deepen,
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub kind: TaskKind,
    pub suite: String,
    pub summary: String,
    pub files: Vec<String>,
    pub evidence: Value,
    pub steps: String,
    pub accept: String,
}

const PER_KIND: usize = 5;

pub fn all(repo: &Path, cfg: &Config, m: &Measurement) -> Result<Vec<Task>> {
    let mut out = Vec::new();
    for (sc, s) in cfg.suites.iter().zip(&m.suites) {
        out.extend(of_measurement(s));
        out.extend(deepen(repo, &Classifier::new(&sc.resolve())?, s)?);
    }
    out.sort_by_key(|t| t.kind);
    Ok(out)
}

/// The tasks a suite's measurement alone shows (everything but `deepen`,
/// which reads git history).
pub fn of_measurement(s: &SuiteMeasurement) -> Vec<Task> {
    [failing(s), uncovered(s), redundant(s), slow(s), surviving(s)].concat()
}

fn failing(s: &SuiteMeasurement) -> Vec<Task> {
    s.tests
        .iter()
        .filter(|t| !t.passed)
        .map(|t| Task {
            kind: TaskKind::FailingTest,
            suite: s.suite.clone(),
            summary: format!("{} fails", t.id),
            files: vec![],
            evidence: json!({ "test": t.id, "log": format!(".slimfit/out/{}/test.log", s.suite) }),
            steps: "Read the log, find why it fails, fix the code (or the test if the test is wrong). Nothing else is measured honestly until every test passes.".into(),
            accept: "slimfit check".into(),
        })
        .collect()
}

fn uncovered(s: &SuiteMeasurement) -> Vec<Task> {
    s.uncovered
        .iter()
        .filter(|sp| sp.lines >= 2)
        .take(PER_KIND)
        .map(|sp| Task {
            kind: TaskKind::Uncovered,
            suite: s.suite.clone(),
            summary: format!(
                "{}:{}-{} ({} lines{}) runs under no test",
                sp.file,
                sp.start,
                sp.end,
                sp.lines,
                sp.function.as_ref().map(|f| format!(", in {f}")).unwrap_or_default()
            ),
            files: vec![sp.file.clone()],
            evidence: json!(sp),
            steps: "Find who calls this code. No caller on any user path: delete it, and whatever only it used. \
                    A user path needs it: add a test through the module's interface that runs it (never test past the interface). \
                    It is a product feature nobody seems to use: do not delete it; removing features is a product decision, so escalate with this evidence."
                .into(),
            accept: "slimfit check".into(),
        })
        .collect()
}

/// Tests whose killed mutants are all killed by a cheaper cover of the
/// rest: a time-weighted greedy set cover over the kill matrix.
/// Not when only the changed files were mutated: a test that kills only
/// mutants of the others would look as if it killed none.
fn redundant(s: &SuiteMeasurement) -> Vec<Task> {
    if s.mutated.is_some() {
        return vec![];
    }
    let killed: Vec<(usize, &Vec<String>)> = s
        .mutants
        .iter()
        .enumerate()
        .filter(|(_, m)| m.status == MutantStatus::Killed)
        .filter_map(|(i, m)| m.killed_by.as_ref().map(|k| (i, k)))
        .collect();
    if killed.is_empty() && s.mutation_tests.is_none() {
        return vec![];
    }
    let mut kills: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for t in s.mutation_tests.iter().flatten() {
        kills.entry(t.clone()).or_default();
    }
    for (i, ks) in &killed {
        for k in *ks {
            kills.entry(k.clone()).or_default().insert(*i);
        }
    }
    let seconds = |name: &str| -> Option<f64> {
        s.tests
            .iter()
            .find(|t| t.id == name || t.id.ends_with(&format!("::{name}")) || t.id.ends_with(&format!(" {name}")))
            .map(|t| t.seconds)
    };
    let mut covered: BTreeSet<usize> = BTreeSet::new();
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    loop {
        let best = kills
            .iter()
            .filter(|(t, _)| !chosen.contains(*t))
            .map(|(t, ks)| (t, ks.difference(&covered).count(), seconds(t).unwrap_or(0.0).max(0.001)))
            .filter(|(_, gain, _)| *gain > 0)
            .max_by(|a, b| (a.1 as f64 / a.2).total_cmp(&(b.1 as f64 / b.2)));
        let Some((t, _, _)) = best else { break };
        covered.extend(&kills[t]);
        chosen.insert(t.clone());
    }
    let mut spare: Vec<(&String, &BTreeSet<usize>, Option<f64>)> =
        kills.iter().filter(|(t, _)| !chosen.contains(*t)).map(|(t, ks)| (t, ks, seconds(t))).collect();
    spare.sort_by(|a, b| b.2.unwrap_or(0.0).total_cmp(&a.2.unwrap_or(0.0)).then(a.0.cmp(b.0)));
    spare
        .into_iter()
        .take(PER_KIND)
        .map(|(t, ks, secs)| Task {
            kind: TaskKind::RedundantTest,
            suite: s.suite.clone(),
            summary: if ks.is_empty() {
                format!("{t} kills no mutant")
            } else {
                format!("{t} kills only mutants other tests also kill ({})", ks.len())
            },
            files: vec![],
            evidence: json!({ "test": t, "seconds": secs, "mutants_killed": ks.len(),
                              "also_killed_by": ks.iter().flat_map(|i| s.mutants[*i].killed_by.iter().flatten()).filter(|k| *k != t).collect::<BTreeSet<_>>() }),
            steps: "Read the test. If it checks behaviour no other test pins down (a user-visible outcome the mutants here don't reach), keep it and strengthen the code it covers instead. \
                    Otherwise delete it. The mutation bound proves nothing was lost: the accept command mutates every file, since a deleted test can let mutants of unchanged files survive."
                .into(),
            accept: "slimfit check --all-mutants".into(),
        })
        .collect()
}

fn slow(s: &SuiteMeasurement) -> Vec<Task> {
    let total = s.metrics.test_seconds;
    let mut tests: Vec<_> = s.tests.iter().filter(|t| t.seconds >= 0.5 && t.seconds >= total * 0.05).collect();
    tests.sort_by(|a, b| b.seconds.total_cmp(&a.seconds));
    tests
        .into_iter()
        .take(PER_KIND)
        .map(|t| Task {
            kind: TaskKind::SlowTest,
            suite: s.suite.clone(),
            summary: format!("{} takes {:.2}s of {:.2}s", t.id, t.seconds, total),
            files: vec![],
            evidence: json!({ "test": t.id, "seconds": t.seconds, "suite_seconds": total }),
            steps: "Find where the time goes (sleeps, real network or disk, process spawns, large fixtures, repeated setup). \
                    Make it cheaper without testing less: shrink the input, share setup, replace a slow dependency with an in-memory adapter at an existing seam. \
                    If it overlaps faster tests, merge or delete it."
                .into(),
            accept: "slimfit check --mutants".into(),
        })
        .collect()
}

fn surviving(s: &SuiteMeasurement) -> Vec<Task> {
    let mut by_file: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for m in s.mutants.iter().filter(|m| m.status == MutantStatus::Survived) {
        by_file.entry(&m.file).or_default().push(json!({ "line": m.line, "mutant": m.description }));
    }
    let mut files: Vec<_> = by_file.into_iter().collect();
    files.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    files
        .into_iter()
        .take(PER_KIND)
        .map(|(file, ms)| Task {
            kind: TaskKind::SurvivingMutant,
            suite: s.suite.clone(),
            summary: format!("{} mutants survive in {file}", ms.len()),
            files: vec![file.to_string()],
            evidence: json!({ "survivors": ms }),
            steps: "For each survivor: the change it makes is behaviour no test checks. If the behaviour matters, add an assertion through the module's interface that the mutant breaks. \
                    If it does not matter, the code is more than it needs to be: simplify it until the mutant cannot exist."
                .into(),
            accept: "slimfit check --mutants".into(),
        })
        .collect()
}

/// Source files that keep changing and are big: where a deeper module pays
/// off soonest.
fn deepen(repo: &Path, classifier: &Classifier, s: &SuiteMeasurement) -> Result<Vec<Task>> {
    let log =
        git::run(repo, &["log", "--since=90.days", "--no-merges", "--format=", "--name-only"]).unwrap_or_default();
    let mut changes: BTreeMap<&str, u64> = BTreeMap::new();
    for f in log.lines().filter(|l| !l.is_empty()) {
        if classifier.kind(f) == Some(Kind::Source) {
            *changes.entry(f).or_default() += 1;
        }
    }
    let mut hot = Vec::new();
    for (f, n) in changes.into_iter().filter(|(_, n)| *n >= 5) {
        let Ok(text) = std::fs::read_to_string(repo.join(f)) else { continue };
        let lines = classifier.lines(Kind::Source, &text).prod;
        if lines >= 100 {
            hot.push((f.to_string(), n, lines));
        }
    }
    hot.sort_by_key(|(f, n, l)| (std::cmp::Reverse(n * l), f.clone()));
    Ok(hot
        .into_iter()
        .take(3)
        .map(|(f, n, l)| Task {
            kind: TaskKind::Deepen,
            suite: s.suite.clone(),
            summary: format!("{f}: {l} lines, changed in {n} commits in 90 days"),
            files: vec![f.clone()],
            evidence: json!({ "file": f, "prod_lines": l, "commits_90d": n }),
            steps: "Load the codebase-design and improve-codebase-architecture skills. Look at the module this file belongs to: \
                    shallow interfaces, pass-through functions, one concept spread over many small modules, tests that reach past the interface. \
                    Write the deeper interface down first (GLOSSARY names, ADR if it settles a trade-off), then move to it one step per commit, each step passing the accept command."
                .into(),
            accept: "slimfit check --mutants".into(),
        })
        .collect())
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::model::{Metrics, Mutant, TestCase};

    fn mutant(status: MutantStatus, killers: &[&str]) -> Mutant {
        Mutant {
            file: "a".into(),
            line: 1,
            description: "m".into(),
            status,
            killed_by: Some(killers.iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn redundant_tests_are_those_outside_the_cheapest_cover() {
        let s = SuiteMeasurement {
            suite: "s".into(),
            metrics: Metrics::default(),
            tests: ["slow", "fast", "idle"]
                .iter()
                .zip([3.0, 0.1, 0.1])
                .map(|(n, secs)| TestCase { id: format!("x::{n}"), seconds: secs, passed: true })
                .collect(),
            uncovered: vec![],
            mutants: vec![
                mutant(MutantStatus::Killed, &["slow", "fast"]),
                mutant(MutantStatus::Killed, &["fast"]),
                mutant(MutantStatus::Survived, &[]),
            ],
            mutation_tests: Some(vec!["slow".into(), "fast".into(), "idle".into()]),
            mutated: None,
            command_seconds: 0.0,
        };
        let tasks = redundant(&s);
        let names: Vec<&str> = tasks.iter().map(|t| t.evidence["test"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["slow", "idle"]);
        assert!(tasks[1].summary.contains("kills no mutant"));
    }
}
