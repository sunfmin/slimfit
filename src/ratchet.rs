//! The baseline (`.slimfit/baseline.json`, committed) and the two things
//! done with it: `check` a measurement against it, and `ratchet` it to a
//! better measurement. Bounds only tighten unless loosened with a reason.

use crate::config::CheckConfig;
use crate::fault::{Fault, Faults, fault};
use crate::model::{Measurement, Metrics, Snapshot, SuiteMeasurement};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = ".slimfit/baseline.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    pub suites: BTreeMap<String, Metrics>,
    /// Every time a bound was raised, and why.
    #[serde(default)]
    pub loosened: Vec<Loosening>,
    /// The tree mutants were last measured at: `--mutants` runs the mutants
    /// of the lines changed since (ADR 0005). `init` sets it, so adopting
    /// slimfit runs no mutant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutants_at: Option<Snapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Loosening {
    pub suite: String,
    pub metric: Bound,
    pub from: f64,
    pub to: f64,
    pub reason: String,
}

/// The bounded metrics. Line counts are recorded but not bounded: code may
/// grow, as long as every new line is covered and every new mutant killed.
/// Each variant's help is the rule `check` holds it to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum, agent_help::Help)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Bound {
    /// Failing tests: at most the baseline's (none, once every test passes); never loosened.
    #[value(verbatim_doc_comment)]
    Failures,
    /// The sum of the tests' own durations, build time excluded, corrected for the host's load (see [host load](help:host-load)): at most baseline × (1 + `time_tolerance`) + `time_slack` (`slimfit.toml`'s `[check]`; by default {const:TIME_TOLERANCE} and {const:TIME_SLACK} seconds).
    #[value(verbatim_doc_comment)]
    TestSeconds,
    /// Lines no test runs: at most baseline + `coverage_slack` (by default {const:COVERAGE_SLACK}): code may grow, untested code may not. A drop counts as a gain only past the slack, so a line whose coverage flaps by timing never fails the gate.
    #[value(verbatim_doc_comment)]
    UncoveredLines,
    /// Mutants no test kills in the changed lines: none, checked only with `--mutants` (see [mutants scope](help:mutants-scope)). Survivors elsewhere are tasks, not violations; no baseline keeps a count.
    #[value(verbatim_doc_comment)]
    SurvivedMutants,
}

/// What a [`Loosening`] prints, as the command pages name it.
pub const LOOSENING_HELP: &str =
    "each with `suite`, `metric`, `from` (the bound before), `to` (the bound after) and `reason`";

/// What a [`Violation`] prints, as the command pages name it.
pub const VIOLATION_HELP: &str =
    "each with `suite`, `metric`, `baseline`, `limit` (the most it may be), `current` and `fix` (what to do)";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Violation {
    pub suite: String,
    pub metric: Bound,
    pub baseline: f64,
    pub limit: f64,
    pub current: f64,
    pub fix: String,
}

#[cfg(test)]
impl Violation {
    /// A violation of no substance: what help lists the keys of.
    pub fn example() -> Self {
        Violation {
            suite: String::new(),
            metric: Bound::Failures,
            baseline: 0.0,
            limit: 0.0,
            current: 0.0,
            fix: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Change {
    pub suite: String,
    pub metric: Bound,
    pub baseline: f64,
    pub current: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub ok: bool,
    pub violations: Vec<Violation>,
    /// Bounded metrics that got better: `slimfit ratchet` locks them in.
    pub improved: Vec<Change>,
    /// Suites with no baseline yet: `slimfit ratchet` records them.
    pub new_suites: Vec<String>,
    /// Mutants were not measured, so their bound was not checked.
    pub mutants_unchecked: bool,
}

pub fn load(repo: &Path) -> Result<Baseline> {
    match std::fs::read_to_string(repo.join(FILE)) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| {
            fault(
                format!("{FILE} is not a baseline: {e}"),
                format!("restore it: `git checkout -- {FILE}`; with none committed, delete it and run `slimfit init`"),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Baseline::default()),
        Err(e) => Err(e.into()),
    }
}

/// The baseline file's text.
pub fn render(b: &Baseline) -> Result<String> {
    Ok(serde_json::to_string_pretty(b)? + "\n")
}

/// A bound's value in a baseline's (or a measurement's) metrics; none for
/// `survived_mutants`, which no baseline keeps.
fn value(m: &Metrics, b: Bound) -> Option<f64> {
    Some(match b {
        Bound::Failures => m.failures as f64,
        Bound::TestSeconds => m.test_seconds,
        Bound::UncoveredLines => m.uncovered_lines as f64,
        Bound::SurvivedMutants => return None,
    })
}

/// The mutants that survive in the lines of the change, when measured.
fn survivors(s: &SuiteMeasurement) -> Option<f64> {
    s.mutated.as_ref().map(|m| m.change.survived as f64)
}

fn limit(cfg: &CheckConfig, b: Bound, base: f64) -> f64 {
    match b {
        Bound::TestSeconds => base * (1.0 + cfg.time_tolerance) + cfg.time_slack,
        Bound::UncoveredLines => base + cfg.coverage_slack as f64,
        _ => base,
    }
}

/// Below this a bound has improved. Coverage must drop by more than the
/// slack, or a line that flaps would be locked in and then fail the next run.
fn improved_below(cfg: &CheckConfig, b: Bound, base: f64) -> f64 {
    match b {
        Bound::UncoveredLines => base - cfg.coverage_slack as f64,
        _ => base,
    }
}

fn fix(b: Bound, s: &SuiteMeasurement) -> String {
    let list = |kind: &str| format!("`slimfit next --all --kind {kind}` lists them");
    match b {
        Bound::Failures => {
            let failing: Vec<&str> = s.tests.iter().filter(|t| !t.passed).map(|t| t.id.as_str()).take(10).collect();
            format!("make these tests pass: {}; {}", failing.join(", "), list("failing-test"))
        }
        Bound::TestSeconds => format!(
            "the tests got slower: make the new slow tests cheaper, or move them behind a faster adapter; {}",
            list("slow-test")
        ),
        Bound::UncoveredLines => format!(
            "new lines run under no test: cover each through its module's interface, or delete it; {}",
            list("uncovered")
        ),
        Bound::SurvivedMutants => format!(
            "mutants survive in the changed lines: assert the behaviour each one changes, or simplify the code until it cannot exist; {}",
            list("surviving-mutant")
        ),
    }
}

pub fn check(base: &Baseline, m: &Measurement, cfg: &CheckConfig) -> Verdict {
    let mut v = Verdict {
        ok: true,
        violations: vec![],
        improved: vec![],
        new_suites: vec![],
        mutants_unchecked: !m.with_mutants,
    };
    for s in &m.suites {
        if let Some(n) = survivors(s).filter(|n| *n > 0.0) {
            v.violations.push(Violation {
                suite: s.suite.clone(),
                metric: Bound::SurvivedMutants,
                baseline: 0.0,
                limit: 0.0,
                current: n,
                fix: fix(Bound::SurvivedMutants, s),
            });
        }
        let Some(b) = base.suites.get(&s.suite) else {
            v.new_suites.push(s.suite.clone());
            if s.metrics.failures > 0 {
                v.violations.push(Violation {
                    suite: s.suite.clone(),
                    metric: Bound::Failures,
                    baseline: 0.0,
                    limit: 0.0,
                    current: s.metrics.failures as f64,
                    fix: fix(Bound::Failures, s),
                });
            }
            continue;
        };
        for &bound in <Bound as clap::ValueEnum>::value_variants() {
            let (Some(base_v), Some(cur)) = (value(b, bound), value(&s.metrics, bound)) else { continue };
            let lim = if bound == Bound::Failures { 0.0 } else { limit(cfg, bound, base_v) };
            if cur > lim + 1e-9 {
                v.violations.push(Violation {
                    suite: s.suite.clone(),
                    metric: bound,
                    baseline: base_v,
                    limit: lim,
                    current: cur,
                    fix: fix(bound, s),
                });
            } else if cur < improved_below(cfg, bound, base_v) - 1e-9 && bound != Bound::Failures {
                v.improved.push(Change { suite: s.suite.clone(), metric: bound, baseline: base_v, current: cur });
            }
        }
    }
    v.ok = v.violations.is_empty();
    v
}

/// The new baseline: each bound at the better of baseline and current;
/// a loosened bound at current, recorded with its reason. Refuses while
/// any other bound is violated. With mutants measured, `mutants_at` moves
/// to the tree measured: its change is accepted.
pub fn ratchet(
    base: &Baseline,
    m: &Measurement,
    cfg: &CheckConfig,
    loosen: &[Bound],
    reason: Option<&str>,
) -> Result<Baseline> {
    let verdict = check(base, m, cfg);
    let blocking: Vec<&Violation> =
        verdict.violations.iter().filter(|v| !loosen.contains(&v.metric) || v.metric == Bound::Failures).collect();
    let mut faults: Vec<Fault> = blocking
        .iter()
        .map(|v| {
            let name = serde_json::to_value(v.metric).unwrap_or_default().as_str().unwrap_or("").to_string();
            let raise = if v.metric == Bound::Failures {
                String::new()
            } else {
                format!("; or, if the rise is right, `slimfit ratchet --loosen {name} --reason \"<why>\"`")
            };
            Fault {
                fault: format!("cannot ratchet: {} {name} is {}, over its bound {}", v.suite, v.current, v.limit),
                fix: format!("{}{raise}", v.fix),
            }
        })
        .collect();
    if !loosen.is_empty() && reason.is_none_or(|r| r.trim().is_empty()) {
        faults.push(Fault {
            fault: "--loosen without --reason".into(),
            fix: "add `--reason \"<why the bound must rise>\"`: it is kept in the baseline for whoever reads it next"
                .into(),
        });
    }
    if !faults.is_empty() {
        return Err(Faults(faults).into());
    }
    let mutants_at = if m.with_mutants { m.snapshot.clone() } else { base.mutants_at.clone() };
    let mut next = Baseline { suites: BTreeMap::new(), loosened: base.loosened.clone(), mutants_at };
    for s in &m.suites {
        let mut metrics = s.metrics.clone();
        if loosen.contains(&Bound::SurvivedMutants)
            && let Some(to) = survivors(s).filter(|n| *n > 0.0)
        {
            next.loosened.push(Loosening {
                suite: s.suite.clone(),
                metric: Bound::SurvivedMutants,
                from: 0.0,
                to,
                reason: reason.unwrap_or_default().into(),
            });
        }
        if let Some(old) = base.suites.get(&s.suite) {
            for &bound in loosen {
                if let (Some(from), Some(to)) = (value(old, bound), value(&metrics, bound))
                    && to > from
                {
                    next.loosened.push(Loosening {
                        suite: s.suite.clone(),
                        metric: bound,
                        from,
                        to,
                        reason: reason.unwrap_or_default().into(),
                    });
                }
            }
            if !loosen.contains(&Bound::TestSeconds) {
                metrics.test_seconds = metrics.test_seconds.min(old.test_seconds);
            }
            if !loosen.contains(&Bound::UncoveredLines)
                && metrics.uncovered_lines as f64
                    >= improved_below(cfg, Bound::UncoveredLines, old.uncovered_lines as f64)
            {
                metrics.uncovered_lines = old.uncovered_lines;
            }
        }
        next.suites.insert(s.suite.clone(), metrics);
    }
    Ok(next)
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::model::{MutantCounts, Mutated};

    fn suite(name: &str, metrics: Metrics, survived: Option<u64>) -> SuiteMeasurement {
        let change = |n| MutantCounts { killed: 3, survived: n, unviable: 0 };
        SuiteMeasurement {
            suite: name.into(),
            metrics,
            tests: vec![],
            uncovered: vec![],
            mutants: vec![],
            test_lines: None,
            mutated: survived.map(|n| Mutated { files: vec!["src/a.rs".into()], change: change(n) }),
            command_seconds: 0.0,
            load_slowdown: 1.0,
        }
    }

    /// `survived`: the change's survivors, when mutants were measured.
    fn measurement(metrics: Metrics, survived: Option<u64>) -> Measurement {
        let snapshot = survived.map(|_| Snapshot { commit: "c".into(), tree: Some("t".into()) });
        Measurement {
            tree: "t".into(),
            with_mutants: survived.is_some(),
            snapshot,
            since: None,
            suites: vec![suite("s", metrics, survived)],
        }
    }

    /// A baseline, and a cached measurement, written before whole-codebase
    /// runs and the kill matrix were removed still load: the removed fields
    /// are ignored, and the next ratchet writes the baseline without them.
    #[test]
    fn old_files_with_removed_fields_still_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".slimfit")).unwrap();
        let old = r#"{"suites":{"rust":{"prod_lines":1,"test_lines":1,"tests":1,"failures":0,"test_seconds":1.0,
            "covered_lines":1,"uncovered_lines":0,"mutants":{"killed":3},"mutant_files":{"a.rs":{}},"mutants_pending":["b.rs"]}},
            "mutants_at":{"commit":"c","dirty":["x"]}}"#;
        std::fs::write(dir.path().join(FILE), old).unwrap();
        let b = load(dir.path()).unwrap();
        assert_eq!((b.suites["rust"].tests, b.mutants_at.unwrap().commit), (1, "c".to_string()));
        let cached = r#"{"tree":"t","with_mutants":true,"suites":[{"suite":"rust","metrics":{"prod_lines":1,"test_lines":1,
            "tests":1,"failures":0,"test_seconds":1.0,"covered_lines":1,"uncovered_lines":0},"tests":[],"uncovered":[],
            "mutants":[{"file":"a.rs","line":1,"description":"d","status":"killed","killed_by":["t"]}],"mutation_tests":["t"],
            "mutated":{"all":true,"files":[],"change":{"killed":0,"survived":0,"unviable":0}},"command_seconds":1.0,"load_slowdown":1.0}]}"#;
        let m: Measurement = serde_json::from_str(cached).unwrap();
        assert_eq!(m.suites[0].mutants.len(), 1);
    }

    fn base(metrics: Metrics) -> Baseline {
        Baseline { suites: [("s".to_string(), metrics)].into(), loosened: vec![], mutants_at: None }
    }

    fn m(uncovered: u64, secs: f64) -> Metrics {
        Metrics { uncovered_lines: uncovered, test_seconds: secs, ..Default::default() }
    }

    #[test]
    fn growth_of_a_bound_is_a_violation_and_shrinking_an_improvement() {
        let cfg = CheckConfig::default();
        let v = check(&base(m(10, 10.0)), &measurement(m(12, 9.0), Some(0)), &cfg);
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), vec![Bound::UncoveredLines]);
        assert!(v.violations[0].fix.ends_with("`slimfit next --all --kind uncovered` lists them"));
        assert_eq!(v.improved.iter().map(|c| c.metric).collect::<Vec<_>>(), vec![Bound::TestSeconds]);
        let v = check(&base(m(10, 10.0)), &measurement(m(10, 13.1), None), &cfg);
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), vec![Bound::TestSeconds]);
        assert!(v.mutants_unchecked);
        // A suite new to the baseline is recorded, but its failures still fail.
        let v = check(&Baseline::default(), &measurement(Metrics { failures: 1, ..m(1, 1.0) }, None), &cfg);
        assert_eq!((v.new_suites, v.violations.len()), (vec!["s".to_string()], 1));
    }

    /// No baseline keeps a mutant count: a survivor in the change fails the
    /// check, in a new suite too, until it is killed or loosened with a
    /// reason. Ratcheting a mutants run moves `mutants_at` to its tree.
    #[test]
    fn the_changes_own_survivors_fail_until_loosened_and_a_ratchet_moves_mutants_at() {
        let cfg = CheckConfig::default();
        let two = measurement(m(10, 10.0), Some(2));
        for b in [base(m(10, 10.0)), Baseline::default()] {
            let v = check(&b, &two, &cfg);
            assert_eq!(
                v.violations.iter().map(|v| (v.metric, v.limit, v.current)).collect::<Vec<_>>(),
                [(Bound::SurvivedMutants, 0.0, 2.0)]
            );
            assert!(ratchet(&b, &two, &cfg, &[], None).is_err());
        }
        let next = ratchet(&base(m(10, 10.0)), &two, &cfg, &[Bound::SurvivedMutants], Some("equivalent")).unwrap();
        let l = &next.loosened[0];
        assert_eq!((l.metric, l.from, l.to, l.reason.as_str()), (Bound::SurvivedMutants, 0.0, 2.0, "equivalent"));
        assert_eq!(next.mutants_at, two.snapshot);
        let clean = ratchet(&base(m(10, 10.0)), &measurement(m(10, 10.0), Some(0)), &cfg, &[], None).unwrap();
        assert!(clean.loosened.is_empty() && clean.mutants_at.is_some());
    }

    #[test]
    fn a_baseline_that_does_not_parse_is_restored_from_git() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), Baseline::default());
        std::fs::create_dir_all(dir.path().join(".slimfit")).unwrap();
        std::fs::write(dir.path().join(FILE), "{").unwrap();
        let f = &crate::fault::of(load(dir.path()).unwrap_err())[0];
        assert!(f.fault.starts_with(".slimfit/baseline.json is not a baseline"), "{f:?}");
        assert!(f.fix.starts_with("restore it: `git checkout -- .slimfit/baseline.json`"), "{f:?}");
    }

    #[test]
    fn ratchet_keeps_the_better_time_and_mutants_at_when_unmeasured() {
        let cfg = CheckConfig::default();
        let at = Some(Snapshot { commit: "old".into(), tree: None });
        let old = Baseline { mutants_at: at.clone(), ..base(m(10, 10.0)) };
        let next = ratchet(&old, &measurement(m(8, 11.0), None), &cfg, &[], None).unwrap();
        let s = &next.suites["s"];
        assert_eq!((s.uncovered_lines, s.test_seconds, next.mutants_at), (8, 10.0, at));
    }

    #[test]
    fn loosening_needs_a_reason_and_is_recorded() {
        let cfg = CheckConfig::default();
        let (b, cur) = (base(m(10, 10.0)), measurement(m(12, 10.0), None));
        let fixes = |r: Result<Baseline>| -> Vec<String> {
            crate::fault::of(r.unwrap_err()).into_iter().map(|f| f.fix).collect()
        };
        let f = fixes(ratchet(&b, &cur, &cfg, &[], None));
        assert!(
            f[0].ends_with("or, if the rise is right, `slimfit ratchet --loosen uncovered_lines --reason \"<why>\"`")
        );
        assert!(fixes(ratchet(&b, &cur, &cfg, &[Bound::UncoveredLines], None))[0].starts_with("add `--reason"));
        // Every refusal at once: each violated bound, and the missing reason.
        let worse = measurement(Metrics { failures: 1, ..m(12, 20.0) }, None);
        let f = fixes(ratchet(&b, &worse, &cfg, &[Bound::TestSeconds], None));
        assert_eq!(f.len(), 3, "{f:?}");
        assert!(f[0].starts_with("make these tests pass") && !f[0].contains("--loosen failures"), "{f:?}");
        assert!(f[1].contains("--loosen uncovered_lines") && f[2].starts_with("add `--reason"), "{f:?}");
        let next = ratchet(&b, &cur, &cfg, &[Bound::UncoveredLines], Some("generated code")).unwrap();
        assert_eq!(next.suites["s"].uncovered_lines, 12);
        assert_eq!(next.loosened[0].reason, "generated code");
        // A loosened bound that did not rise is not recorded as raised.
        let same = ratchet(&b, &measurement(m(10, 10.0), None), &cfg, &[Bound::UncoveredLines], Some("x"));
        assert!(same.unwrap().loosened.is_empty());
    }
}
