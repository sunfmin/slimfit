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
    /// The tree the suites' mutant counts were measured at: `--mutants`
    /// mutates only the files changed since.
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Bound {
    /// Failing tests; never loosened.
    #[value(verbatim_doc_comment)]
    Failures,
    /// The sum of the tests' own durations.
    #[value(verbatim_doc_comment)]
    TestSeconds,
    /// Lines no test runs.
    #[value(verbatim_doc_comment)]
    UncoveredLines,
    /// Mutants no test kills.
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
    /// Per suite, the source files with no mutant count yet: their mutants
    /// are not held. `slimfit ratchet --pending-seconds <s>` measures some.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mutants_pending: BTreeMap<String, Vec<String>>,
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

fn value(m: &Metrics, b: Bound) -> Option<f64> {
    Some(match b {
        Bound::Failures => m.failures as f64,
        Bound::TestSeconds => m.test_seconds,
        Bound::UncoveredLines => m.uncovered_lines as f64,
        Bound::SurvivedMutants => m.mutants?.survived as f64,
    })
}

/// `cur`'s value of `bound` over what `base` holds: the mutants of files
/// the baseline has no count for yet are new, not held.
fn held(base: &Metrics, cur: &Metrics, bound: Bound) -> Option<f64> {
    let v = value(cur, bound)?;
    if bound != Bound::SurvivedMutants {
        return Some(v);
    }
    let fresh: u64 = base.mutants_pending.iter().filter_map(|f| cur.mutant_files.get(f)).map(|c| c.survived).sum();
    Some(v - fresh as f64)
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
        Bound::SurvivedMutants => "more mutants survive: a test lost its teeth or new code is untested; see `slimfit next --all` (kind surviving-mutant)".into(),
    }
}

const BOUNDS: [Bound; 4] = [Bound::Failures, Bound::TestSeconds, Bound::UncoveredLines, Bound::SurvivedMutants];

pub fn check(base: &Baseline, m: &Measurement, cfg: &CheckConfig) -> Verdict {
    let mut v = Verdict {
        ok: true,
        violations: vec![],
        improved: vec![],
        new_suites: vec![],
        mutants_unchecked: !m.with_mutants,
        mutants_pending: BTreeMap::new(),
    };
    for s in &m.suites {
        if m.with_mutants && !s.metrics.mutants_pending.is_empty() {
            v.mutants_pending.insert(s.suite.clone(), s.metrics.mutants_pending.clone());
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
            let (Some(base_v), Some(cur)) = (value(b, bound), held(b, &s.metrics, bound)) else { continue };
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
/// any other bound is violated, except that a piece (`m.piece`: a run that
/// fills in pending mutant counts) is not refused for test_seconds: host
/// load swells it, and the piece's mutant counts must not be thrown away
/// for that. Such a violation is returned with the baseline; test_seconds
/// stays as it was.
pub fn ratchet(
    base: &Baseline,
    m: &Measurement,
    cfg: &CheckConfig,
    loosen: &[Bound],
    reason: Option<&str>,
) -> Result<(Baseline, Vec<Violation>)> {
    let verdict = check(base, m, cfg);
    let (not_recorded, violations): (Vec<Violation>, Vec<Violation>) = verdict
        .violations
        .into_iter()
        .partition(|v| m.piece && v.metric == Bound::TestSeconds && !loosen.contains(&Bound::TestSeconds));
    let blocking: Vec<&Violation> =
        violations.iter().filter(|v| !loosen.contains(&v.metric) || v.metric == Bound::Failures).collect();
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
            if metrics.mutants.is_none() {
                metrics.mutants = old.mutants;
                metrics.mutant_files = old.mutant_files.clone();
                metrics.mutants_pending = old.mutants_pending.clone();
            }
        }
        next.suites.insert(s.suite.clone(), metrics);
    }
    Ok((next, not_recorded))
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::model::MutantCounts;

    fn suite(name: &str, metrics: Metrics) -> SuiteMeasurement {
        SuiteMeasurement {
            suite: name.into(),
            metrics,
            tests: vec![],
            uncovered: vec![],
            mutants: vec![],
            mutation_tests: None,
            mutated: None,
            command_seconds: 0.0,
            load_slowdown: 1.0,
        }
    }

    fn measurement(metrics: Metrics, with_mutants: bool) -> Measurement {
        Measurement { tree: "t".into(), with_mutants, piece: false, snapshot: None, suites: vec![suite("s", metrics)] }
    }

    fn base(metrics: Metrics) -> Baseline {
        Baseline { suites: [("s".to_string(), metrics)].into(), loosened: vec![], mutants_at: None }
    }

    fn m(uncovered: u64, secs: f64, survived: Option<u64>) -> Metrics {
        Metrics {
            uncovered_lines: uncovered,
            test_seconds: secs,
            mutants: survived.map(|s| MutantCounts { killed: 10, survived: s, unviable: 0 }),
            ..Default::default()
        }
    }

    #[test]
    fn growth_of_a_bound_is_a_violation_and_shrinking_an_improvement() {
        let cfg = CheckConfig::default();
        let v = check(&base(m(10, 10.0, Some(2))), &measurement(m(12, 12.9, Some(1)), true), &cfg);
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), vec![Bound::UncoveredLines]);
        assert_eq!(v.improved.len(), 1);
        let v = check(&base(m(10, 10.0, Some(2))), &measurement(m(10, 13.1, None), false), &cfg);
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), vec![Bound::TestSeconds]);
        assert!(v.mutants_unchecked);
        // A suite new to the baseline is recorded, but its failures still fail.
        let v = check(&Baseline::default(), &measurement(Metrics { failures: 1, ..m(1, 1.0, None) }, false), &cfg);
        assert_eq!((v.new_suites, v.violations.len()), (vec!["s".to_string()], 1));
    }

    #[test]
    fn ratchet_keeps_the_better_time_and_old_mutants_when_unmeasured() {
        let cfg = CheckConfig::default();
        let next =
            ratchet(&base(m(10, 10.0, Some(2))), &measurement(m(8, 11.0, None), false), &cfg, &[], None).unwrap().0;
        let s = &next.suites["s"];
        assert_eq!((s.uncovered_lines, s.test_seconds, s.mutants.unwrap().survived), (8, 10.0, 2));
    }

    #[test]
    fn loosening_needs_a_reason_and_is_recorded() {
        let cfg = CheckConfig::default();
        let (b, cur) = (base(m(10, 10.0, None)), measurement(m(12, 10.0, None), false));
        assert!(ratchet(&b, &cur, &cfg, &[], None).is_err());
        assert!(ratchet(&b, &cur, &cfg, &[Bound::UncoveredLines], None).is_err());
        let next = ratchet(&b, &cur, &cfg, &[Bound::UncoveredLines], Some("generated code")).unwrap().0;
        assert_eq!(next.suites["s"].uncovered_lines, 12);
        assert_eq!(next.loosened[0].reason, "generated code");
        // A loosened bound that did not rise is not recorded as raised.
        let same = ratchet(&b, &measurement(m(10, 10.0, None), false), &cfg, &[Bound::UncoveredLines], Some("x"));
        assert!(same.unwrap().0.loosened.is_empty());
    }
}
