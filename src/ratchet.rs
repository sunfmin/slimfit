//! The baseline (`.slimfit/baseline.json`, committed) and the two things
//! done with it: `check` a measurement against it, and `ratchet` it to a
//! better measurement. Bounds only tighten unless loosened with a reason.

use crate::config::CheckConfig;
use crate::model::{Measurement, Metrics, Snapshot, SuiteMeasurement};
use anyhow::{Context, Result, bail};
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
/// `survived_mutants` is not kept in the baseline: it counts the change's
/// own survivors, held at 0.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Bound {
    Failures,
    TestSeconds,
    UncoveredLines,
    SurvivedMutants,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Violation {
    pub suite: String,
    pub metric: Bound,
    pub baseline: f64,
    pub limit: f64,
    pub current: f64,
    pub fix: String,
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
        Ok(t) => serde_json::from_str(&t).with_context(|| format!("reading {FILE}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Baseline::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(repo: &Path, b: &Baseline) -> Result<()> {
    std::fs::create_dir_all(repo.join(".slimfit"))?;
    std::fs::write(repo.join(FILE), serde_json::to_string_pretty(b)? + "\n")?;
    Ok(())
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
    match b {
        Bound::Failures => {
            let failing: Vec<&str> = s.tests.iter().filter(|t| !t.passed).map(|t| t.id.as_str()).take(10).collect();
            format!("make these tests pass: {}", failing.join(", "))
        }
        Bound::TestSeconds => "the tests got slower: find the new slow tests with `slimfit next --all` (kind slow-test) and make them cheaper, or move them behind a faster adapter".into(),
        Bound::UncoveredLines => "new lines run under no test: cover each through its module's interface, or delete it; the spans are in `slimfit next --all` (kind uncovered)".into(),
        Bound::SurvivedMutants => "mutants survive in the lines this change added: assert the behaviour each one changes, or simplify the code until it cannot exist; they are listed by `slimfit next --all --kind surviving-mutant`. One that changes nothing observable: `slimfit ratchet --loosen survived_mutants --reason \"<why>\"`".into(),
    }
}

/// The bounds the baseline keeps a value for.
const BOUNDS: [Bound; 3] = [Bound::Failures, Bound::TestSeconds, Bound::UncoveredLines];

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
        for bound in BOUNDS {
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
    if !blocking.is_empty() {
        let lines: Vec<String> = blocking
            .iter()
            .map(|v| {
                format!(
                    "  {} {}: {} > {} ({})",
                    v.suite,
                    serde_json::to_value(v.metric).unwrap_or_default().as_str().unwrap_or(""),
                    v.current,
                    v.limit,
                    v.fix
                )
            })
            .collect();
        bail!("cannot ratchet while bounds are violated:\n{}", lines.join("\n"));
    }
    if !loosen.is_empty() && reason.is_none_or(|r| r.trim().is_empty()) {
        bail!("--loosen needs --reason: it is recorded in the baseline for whoever reads it next");
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
            mutation_tests: None,
            mutated: survived.map(|n| Mutated { all: false, files: vec!["src/a.rs".into()], change: change(n) }),
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
        assert!(ratchet(&b, &cur, &cfg, &[], None).is_err());
        assert!(ratchet(&b, &cur, &cfg, &[Bound::UncoveredLines], None).is_err());
        let next = ratchet(&b, &cur, &cfg, &[Bound::UncoveredLines], Some("generated code")).unwrap();
        assert_eq!(next.suites["s"].uncovered_lines, 12);
        assert_eq!(next.loosened[0].reason, "generated code");
        // A loosened bound that did not rise is not recorded as raised.
        let same = ratchet(&b, &measurement(m(10, 10.0), None), &cfg, &[Bound::UncoveredLines], Some("x"));
        assert!(same.unwrap().loosened.is_empty());
    }
}
