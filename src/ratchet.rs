//! The baseline (`.slimfit/baseline.json`, committed) and the two things
//! done with it: `check` a measurement against it, and `ratchet` it to a
//! better measurement. Bounds only tighten unless loosened with a reason.

use crate::config::CheckConfig;
use crate::fault::{Fault, Faults, fault};
use crate::landing::Held;
use crate::model::{Measurement, Metrics, ReachTest, Span, SuiteMeasurement};
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
    /// Tests the agent decided to keep as redundant-test or slow-test
    /// candidates (`slimfit keep`), each against its test's text: not
    /// proposed again as that kind until it changes ([`Kept::keeps`]).
    /// Written even when empty, so `report`'s history tells none kept from
    /// a baseline older than verdicts.
    #[serde(default)]
    pub kept: Vec<Kept>,
    /// Deepen candidates the agent judged deep enough (`slimfit keep <file>`),
    /// each against the file's size and churn then: not proposed again until
    /// it has grown or churned well past them ([`KeptFile::holds`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_files: Vec<KeptFile>,
    /// Uncovered spans the agent judged worth no fast test (`slimfit keep
    /// <file>:<start>-<end>`), each against its lines' text: not proposed
    /// again until it changes ([`KeptSpan::holds`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_spans: Vec<KeptSpan>,
    /// Sampled survivors the agent judged equivalent (`slimfit keep
    /// --mutant`): no input tells the mutated code from the original. Each
    /// against its function's text: not proposed, nor counted a survivor,
    /// until it changes ([`KeptMutant::holds`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_mutants: Vec<KeptMutant>,
    /// The commit whose code no landing is charged to a mutants run for:
    /// what was there before the rule came in (ADR 0008). It counts only
    /// while the target has not reached it; set by hand, once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutants_from: Option<String>,
    /// What the measurement the baseline was last written from measured of
    /// each suite's trends, beside the bounds it keeps: those only tighten,
    /// so a rise shows here, landing by landing.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub measured: BTreeMap<String, Measured>,
}

impl Baseline {
    /// The baseline that follows `old` with `suites`' bounds, written from
    /// measurement `m`: the only place one is built from another. The
    /// verdicts carry over (loosenings, kept files, `mutants_from`, and each
    /// keep verdict whose test is unchanged); the trends are `m`'s.
    pub fn next(old: &Baseline, suites: BTreeMap<String, Metrics>, m: &Measurement) -> Baseline {
        // A keep verdict whose test changed, or is gone, holds no more.
        let kept = old
            .kept
            .iter()
            .filter(|k| {
                let Some(s) = m.suites.iter().find(|s| s.suite == k.suite) else { return true };
                let Some(reach) = &s.reach else { return true };
                reach.tests.iter().any(|t| k.holds(&s.suite, t))
            })
            .cloned()
            .collect();
        // A kept span no uncovered span of its suite is now, as kept, holds no more.
        let mut kept_spans = old.kept_spans.clone();
        kept_spans.retain(|k| {
            m.suites
                .iter()
                .find(|s| s.suite == k.suite)
                .is_none_or(|s| s.uncovered.iter().any(|sp| k.holds(&s.suite, sp)))
        });
        Baseline {
            suites,
            loosened: old.loosened.clone(),
            kept,
            kept_files: old.kept_files.clone(),
            kept_spans,
            kept_mutants: old.kept_mutants.clone(),
            mutants_from: old.mutants_from.clone(),
            measured: Measured::of(m),
        }
    }
}

/// A suite's trends as measured, not as ratcheted: `uncovered_lines`, and
/// `test_seconds` when the tests were timed (not run inside `--mutants`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Measured {
    pub uncovered_lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_seconds: Option<f64>,
    /// Why `test_seconds` is absent, when the measurement held no time: a
    /// landing not timed, told from one whose time was dropped unsaid
    /// (baselines before slimfit#74).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untimed: Option<String>,
}

/// Why a measurement holds no `test_seconds` of a suite.
pub const UNTIMED: &str = "its tests ran inside `--mutants`, at background priority, and no test run of a gate's \
     priority timed the same files";

impl Measured {
    /// Each suite's trends in `m`.
    pub fn of(m: &Measurement) -> BTreeMap<String, Measured> {
        let of = |s: &SuiteMeasurement| Measured {
            uncovered_lines: s.metrics.uncovered_lines,
            test_seconds: (!s.untimed).then_some(s.metrics.test_seconds),
            untimed: s.untimed.then(|| UNTIMED.to_string()),
        };
        m.suites.iter().map(|s| (s.suite.clone(), of(s))).collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Kept {
    pub suite: String,
    pub test: String,
    /// The kind of task it answers; a baseline from before kinds holds
    /// only redundant-test verdicts.
    #[serde(default, skip_serializing_if = "KeepKind::is_redundant")]
    pub kind: KeepKind,
    /// The hash of the test's text when it was kept (`TestSource::hash`).
    pub hash: String,
    /// Its CPU seconds when kept: a slow-test verdict's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    pub reason: String,
}

/// The kinds of task a test's keep answers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum KeepKind {
    #[default]
    RedundantTest,
    SlowTest,
}

impl KeepKind {
    fn is_redundant(&self) -> bool {
        *self == KeepKind::RedundantTest
    }
}

/// How much a slow test kept may slow down before it is proposed again:
/// half as much again.
pub const KEPT_SLOWDOWN: f64 = 0.5;

impl Kept {
    /// Whether this verdict's test is `test` of `suite`, its text unchanged.
    pub fn holds(&self, suite: &str, test: &ReachTest) -> bool {
        self.suite == suite && self.test == test.id() && test.source.as_ref().is_some_and(|s| s.hash == self.hash)
    }

    /// Whether it keeps `test` of `suite`, now taking `cpu` seconds, from
    /// `kind`'s tasks.
    pub fn keeps(&self, kind: KeepKind, suite: &str, test: &ReachTest, cpu: f64) -> bool {
        self.kind == kind
            && self.holds(suite, test)
            && self.seconds.is_none_or(|kept| cpu <= kept * (1.0 + KEPT_SLOWDOWN))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeptFile {
    pub suite: String,
    pub file: String,
    /// Its prod lines when it was kept.
    pub prod_lines: u64,
    /// The commits that changed it in the `DEEPEN_DAYS` before it was kept.
    pub commits_90d: u64,
    /// The commits that ever changed it, when it was kept.
    pub commits_total: u64,
    pub reason: String,
}

/// How much a kept file's prod lines may grow before it is proposed again:
/// half as many again.
pub const KEPT_GROWTH: f64 = 0.5;
/// How many commits may change a kept file before it is proposed again, as
/// a share of the commits that made it a candidate: as many again.
pub const KEPT_CHURN: f64 = 1.0;

impl KeptFile {
    /// Whether this verdict still holds for `file` of `suite`, now
    /// `prod_lines` long and changed by `commits_total` commits in all.
    pub fn holds(&self, suite: &str, file: &str, prod_lines: u64, commits_total: u64) -> bool {
        let since = commits_total.saturating_sub(self.commits_total) as f64;
        self.suite == suite
            && self.file == file
            && (prod_lines as f64) < self.prod_lines as f64 * (1.0 + KEPT_GROWTH)
            && since < self.commits_90d as f64 * KEPT_CHURN
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeptSpan {
    pub suite: String,
    pub file: String,
    pub start: u32,
    pub end: u32,
    /// The function it starts in, when the coverage report names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// The hash of its lines' text when it was kept (`Span::hash`).
    pub hash: String,
    pub reason: String,
}

impl KeptSpan {
    /// Whether this verdict still holds for uncovered span `sp` of `suite`:
    /// the same lines of the same function, their text unchanged.
    pub fn holds(&self, suite: &str, sp: &Span) -> bool {
        self.suite == suite
            && (self.file.as_str(), self.start, self.end, &self.function)
                == (sp.file.as_str(), sp.start, sp.end, &sp.function)
            && sp.hash.as_ref() == Some(&self.hash)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeptMutant {
    pub suite: String,
    /// Its name without its line and column (`model::unplaced`): an edit
    /// above it moves it, and the verdict stands.
    pub mutant: String,
    /// The hash of its function's text when it was kept
    /// (`measure::function_hash`).
    pub hash: String,
    pub reason: String,
}

impl KeptMutant {
    /// Whether it names mutant `name` of `suite`, wherever it is now.
    pub fn names(&self, suite: &str, name: &str) -> bool {
        self.suite == suite && self.mutant == crate::model::unplaced(name)
    }

    /// Whether it still keeps mutant `name` of `suite`, whose function's
    /// text now hashes to `hash`.
    pub fn holds(&self, suite: &str, name: &str, hash: &str) -> bool {
        self.names(suite, name) && self.hash == hash
    }
}

/// What a [`KeptMutant`] prints, as the command pages name it.
pub const KEPT_MUTANT_HELP: &str = "each with `suite`, `mutant` (its name without its line and column), `hash` (of its function's text when kept) and `reason`";

/// What a [`KeptSpan`] prints, as the command pages name it.
pub const KEPT_SPAN_HELP: &str = "each with `suite`, `file`, `start`, `end`, `function` (when known), `hash` (of its lines' text when kept) and `reason`";

/// What a [`KeptFile`] prints, as the command pages name it.
pub const KEPT_FILE_HELP: &str = "each with `suite`, `file`, its `prod_lines`, `commits_90d` and `commits_total` (every commit that changed it) when kept, and `reason`";

/// What a [`Kept`] prints, as the command pages name it.
pub const KEPT_HELP: &str = "each with `suite`, `test`, `kind` (`slow-test`; absent for `redundant-test`), `hash` (of the test's text when kept), `seconds` (its CPU seconds when kept, for `slow-test`) and `reason`";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Loosening {
    pub suite: String,
    pub metric: Bound,
    pub from: f64,
    pub to: f64,
    pub reason: String,
}

/// The metrics `check` holds against the baseline. Line counts are recorded
/// but not bounded: code may grow, as long as no test fails and every
/// picked mutant of it is killed. `test_seconds` and `uncovered_lines` are
/// held as trends, not bounds (ADR 0001, 0009, 0010). Each variant's help
/// is the rule `check` holds it to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum, agent_help::Help)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Bound {
    /// Failing tests: none, whatever the baseline records (`init` records failing tests as they are); never loosened.
    #[value(verbatim_doc_comment)]
    Failures,
    /// The CPU time (user + sys) the tests' own processes spent, in seconds, build excluded: each test's own process and those it starts where the test command records it (the Rust preset's runner), otherwise the test command's process tree after the suite's `build_command`. A trend, not a bound: `check` answers it against the baseline and never fails on it, since on a host with fast and slow cores it moves with load (see [host load](help:host-load)); `slimfit ratchet` keeps the lower of the two, `slimfit report` shows it over time and `slow-test` tasks rank by it; never loosened, as there is nothing to loosen. What bounds a gate's time is the gate's own limit (see [factorium](help:factorium)).
    #[value(verbatim_doc_comment)]
    TestSeconds,
    /// Lines no test runs. A trend, not a bound: `check` answers it against the baseline and never fails on it, and when it rose names the uncovered spans the change adds (`new_spans`), for its author to judge: a bound would count a test that runs the lines and asserts nothing as a gain, which only a mutant can tell from a real one. `slimfit ratchet` keeps the lower of the two, a drop counting only past `coverage_slack` (by default {const:COVERAGE_SLACK}) so a line whose coverage flaps by timing is not locked in; `slimfit report` shows it over time and `uncovered` tasks list the spans; never loosened, as there is nothing to loosen.
    #[value(verbatim_doc_comment)]
    UncoveredLines,
    /// Picked mutants of the change no test kills: none, checked only with `--mutants <name>...` (see [mutants scope](help:mutants-scope)). No baseline keeps a count.
    #[value(verbatim_doc_comment)]
    SurvivedMutants,
    /// Source lines the change adds while it adds no record of its mutants (picks, or a reason none is worth one) that names a picker other than its author: none, checked by every `check`, plain ones too, though no mutant runs there (see [what the gate holds](help:mutants-scope)); never loosened.
    #[value(verbatim_doc_comment)]
    UnrecordedMutants,
    /// Source lines the change adds net, in all suites, past `growth_allowance` under `[check]` (by default {const:GROWTH_ALLOWANCE}), while it adds no growth record that names a reviewer other than its author: checked by every `check`, of the change as a whole; never on the number alone, and never loosened (see [the reviewer](help:reviewer)). Its `limit` is the allowance and `suite` names the suites that grew.
    #[value(verbatim_doc_comment)]
    UnrecordedGrowth,
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
    /// Suites whose `test_seconds` was not checked: their tests ran inside
    /// `--mutants` (see [`SuiteMeasurement::untimed`]).
    pub time_unchecked: Vec<String>,
    /// Each timed suite's `test_seconds` against the baseline's: a trend,
    /// which fails nothing.
    pub test_seconds: Vec<Trend>,
    /// Each suite's `uncovered_lines` against the baseline's: a trend, which
    /// fails nothing; where it rose, with the spans the change adds.
    pub uncovered_lines: Vec<Trend>,
}

/// A metric shown against the baseline, which fails nothing.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Trend {
    pub suite: String,
    /// None for a suite new to the baseline.
    pub baseline: Option<f64>,
    pub current: f64,
    /// For `uncovered_lines` that rose: the uncovered spans holding a line
    /// the change adds.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub new_spans: Vec<Span>,
}

impl Trend {
    fn of(suite: &str, baseline: Option<f64>, current: f64) -> Self {
        Trend { suite: suite.into(), baseline, current, new_spans: vec![] }
    }

    fn rose(&self) -> bool {
        self.baseline.is_none_or(|b| self.current > b + 1e-9)
    }
}

/// What a [`Trend`] prints, as the command pages name it.
pub const TREND_HELP: &str = "each with `suite`, `baseline` (`null` for a suite with no baseline yet) and `current`";

/// What `uncovered_lines`' trend prints besides.
pub const NEW_SPANS_HELP: &str = "where it rose, `new_spans`: each uncovered span that holds a line the change \
     adds since `landing.from`, with `file`, `start`, `end`, `lines`, `function`, when known, and `hash`, of its lines' text (none without a landing)";

impl Trend {
    /// Names, when it rose, the uncovered spans of `s` holding a line of
    /// `added` (the landing's lines, by file).
    fn name_new_spans(&mut self, s: &SuiteMeasurement, added: &crate::landing::Lines) {
        if self.rose() {
            self.new_spans = s.uncovered.iter().filter(|sp| sp.holds_any(added)).cloned().collect();
        }
    }
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

/// The mutants that survive in the lines of the change, when measured.
fn survivors(s: &SuiteMeasurement) -> Option<f64> {
    s.mutated.as_ref().map(|m| m.change.survived as f64)
}

/// Each bound's rule, which `check` and `ratchet` both derive from: what the
/// baseline keeps of it, what a measurement holds, its limit (none for a trend), when it is a
/// gain (what `check` calls improved and `ratchet` locks in) and whether it
/// may be loosened.
impl Bound {
    /// What the baseline keeps of it: a suite's metrics, none for a suite
    /// new to the baseline; 0 for the mutants bounds, which no baseline keeps.
    fn kept(self, b: Option<&Metrics>) -> Option<f64> {
        match self {
            Bound::Failures => b.map(|m| m.failures as f64),
            Bound::TestSeconds => b.map(|m| m.test_seconds),
            Bound::UncoveredLines => b.map(|m| m.uncovered_lines as f64),
            Bound::SurvivedMutants | Bound::UnrecordedMutants | Bound::UnrecordedGrowth => Some(0.0),
        }
    }

    /// What a measurement holds of it, and the landing `held` (none when
    /// there is no branch it lands on); none when it was not measured: the
    /// time of tests run inside `--mutants`, the survivors of a run without
    /// picks, and `unrecorded_mutants` without a landing.
    fn current(self, s: &SuiteMeasurement, held: Option<&Held>) -> Option<f64> {
        match self {
            Bound::TestSeconds if s.untimed => None,
            Bound::SurvivedMutants => survivors(s),
            Bound::UnrecordedMutants => {
                held.map(|h| h.unrecorded.get(&s.suite).map_or(0.0, |u| (u.lines + u.tests) as f64))
            }
            // Of the change as a whole, not of a suite: `check` holds it once.
            Bound::UnrecordedGrowth => None,
            _ => self.kept(Some(&s.metrics)),
        }
    }

    /// Writes what the new baseline keeps of it; no-op for those it keeps none of.
    fn keep(self, m: &mut Metrics, v: f64) {
        match self {
            Bound::Failures => m.failures = v as u64,
            Bound::TestSeconds => m.test_seconds = v,
            Bound::UncoveredLines => m.uncovered_lines = v as u64,
            Bound::SurvivedMutants | Bound::UnrecordedMutants | Bound::UnrecordedGrowth => {}
        }
    }

    /// The most it may be, whatever the baseline keeps; none for a trend.
    fn limit(self) -> Option<f64> {
        match self {
            Bound::TestSeconds | Bound::UncoveredLines | Bound::UnrecordedGrowth => None,
            Bound::Failures | Bound::SurvivedMutants | Bound::UnrecordedMutants => Some(0.0),
        }
    }

    /// Whether `current` is a gain over what the baseline keeps. Coverage
    /// must drop by more than the slack, or a line that flaps would be
    /// locked in and then fail the next run.
    fn gain(self, cfg: &CheckConfig, kept: f64, current: f64) -> bool {
        let below = match self {
            Bound::UncoveredLines => kept - cfg.coverage_slack as f64,
            _ => kept,
        };
        current < below - 1e-9
    }

    /// Why it is never loosened, as the fix to do instead; none when it may be.
    fn never_loosened(self) -> Option<String> {
        match self {
            Bound::Failures => Some(format!("make the failing tests pass, or delete them; {}", list("failing-test"))),
            Bound::UnrecordedMutants => Some(format!(
                "{PICK_THE_MUTANTS}; a survivor that changes nothing observable is loosened as `--loosen survived_mutants`"
            )),
            Bound::UnrecordedGrowth => Some(crate::growth::JUSTIFY_GROWTH.into()),
            Bound::TestSeconds => Some(SLOW_TESTS.into()),
            Bound::UncoveredLines => Some(UNCOVERED.into()),
            Bound::SurvivedMutants => None,
        }
    }

    fn name(self) -> String {
        serde_json::to_value(self).unwrap_or_default().as_str().unwrap_or("").to_string()
    }
}

fn list(kind: &str) -> String {
    format!("`slimfit next --all --kind {kind}` lists them")
}

fn fix(b: Bound, s: &SuiteMeasurement, held: Option<&Held>) -> String {
    match b {
        Bound::Failures => {
            let failing: Vec<&str> = s.tests.iter().filter(|t| !t.passed).map(|t| t.id.as_str()).take(10).collect();
            format!("make these tests pass: {}; {}", failing.join(", "), list("failing-test"))
        }
        Bound::TestSeconds => SLOW_TESTS.into(),
        Bound::UncoveredLines => UNCOVERED.into(),
        Bound::SurvivedMutants => format!(
            "picked mutants of the change survive: assert the behaviour each one changes, or simplify the code until it cannot exist; {}",
            list("surviving-mutant")
        ),
        Bound::UnrecordedMutants => {
            let h = held.expect("unrecorded_mutants is measured of a landing");
            let u = &h.unrecorded[&s.suite];
            let missing = if h.records.is_empty() {
                "no record of its mutants"
            } else {
                "no record of its mutants that names a picker other than its author"
            };
            let what = match (u.lines, u.tests) {
                (n, 0) => format!("adds {n} source line(s)"),
                (0, t) => format!("removes {t} test(s)"),
                (n, t) => format!("adds {n} source line(s) and removes {t} test(s)"),
            };
            format!(
                "the change {what} since {} ({}), the first at {}, and {missing}: {PICK_THE_MUTANTS}",
                h.from, h.target, u.first
            )
        }
        Bound::UnrecordedGrowth => unreachable!("held of the change as a whole: `growth_violation`"),
    }
}

/// What to do about `test_seconds`, which bounds nothing.
const SLOW_TESTS: &str = "test_seconds is a trend, not a bound: `slimfit check` never fails on it; to lower it, \
     make the slow tests cheaper (`slimfit next --all --kind slow-test` lists them)";

/// What to do about `uncovered_lines`, which bounds nothing.
pub const UNCOVERED: &str = "uncovered_lines is a trend, not a bound: `slimfit check` never fails on it; to lower \
     it, add a fast test through the module's interface that asserts what the lines decide, or delete the lines if \
     nothing needs them; a test that only runs them is no fix (`slimfit next --all --kind uncovered` lists them)";

/// What clears `unrecorded_mutants`: the rule of every change to source.
pub const PICK_THE_MUTANTS: &str = "list the change's mutants (`slimfit check --mutants`) and ask a second agent \
     to pick, never yourself: give it the goal (`slimfit help picker`), the change's diff and the candidates, nothing \
     of your reasoning; run its picks at background priority, never in a gate: `factorium worker exclusive \
     --background -- slimfit check --mutants <name>... --why <why>...` (outside factorium, `nice -n 19 slimfit check \
     ...`), then `slimfit ratchet --picker <its id> --author <yours>`; or, when it says nothing in the change is worth \
     a mutant, `slimfit ratchet --no-mutants \"<its why>\" --picker <its id> --author <yours>`; then commit \
     .slimfit/mutants/ with the change";

/// What times a tree's tests after `--mutants` ran them (`untimed`).
pub const TIME_THE_TESTS: &str =
    "run a plain `slimfit check` (not at background priority): it runs the tests again and keeps the mutants";

/// The verdict of `m` against `base`, and of the landing `held` when there
/// is a branch it lands on: its records, and the uncovered spans it adds.
pub fn check(base: &Baseline, m: &Measurement, cfg: &CheckConfig, held: Option<&Held>) -> Verdict {
    let mut v = Verdict {
        ok: true,
        violations: vec![],
        improved: vec![],
        new_suites: vec![],
        mutants_unchecked: !m.with_mutants,
        time_unchecked: m.suites.iter().filter(|s| s.untimed).map(|s| s.suite.clone()).collect(),
        test_seconds: vec![],
        uncovered_lines: vec![],
    };
    for s in &m.suites {
        let kept = base.suites.get(&s.suite);
        if kept.is_none() {
            v.new_suites.push(s.suite.clone());
        }
        for &bound in <Bound as clap::ValueEnum>::value_variants() {
            let Some(cur) = bound.current(s, held) else { continue };
            let was = bound.kept(kept);
            match bound {
                Bound::TestSeconds => v.test_seconds.push(Trend::of(&s.suite, was, cur)),
                Bound::UncoveredLines => {
                    let mut t = Trend::of(&s.suite, was, cur);
                    if let Some(h) = held {
                        t.name_new_spans(s, &h.added);
                    }
                    v.uncovered_lines.push(t);
                }
                _ => {}
            }
            if let Some(lim) = bound.limit()
                && cur > lim + 1e-9
            {
                v.violations.push(Violation {
                    suite: s.suite.clone(),
                    metric: bound,
                    baseline: was.unwrap_or(0.0),
                    limit: lim,
                    current: cur,
                    fix: fix(bound, s, held),
                });
            } else if let Some(was) = was.filter(|w| bound.gain(cfg, *w, cur)) {
                v.improved.push(Change { suite: s.suite.clone(), metric: bound, baseline: was, current: cur });
            }
        }
    }
    v.violations.extend(held.and_then(|h| growth_violation(h, cfg.growth_allowance)));
    v.ok = v.violations.is_empty();
    v
}

/// `unrecorded_growth` of the landing `h`: when it adds more source lines net
/// than `allowance` and no growth record naming a reviewer other than its
/// author (ADR 0012).
fn growth_violation(h: &Held, allowance: u64) -> Option<Violation> {
    if !h.growth_due(allowance) {
        return None;
    }
    let grew: Vec<&str> =
        h.grown.iter().filter(|(_, f)| f.values().sum::<i64>() > 0).map(|(s, _)| s.as_str()).collect();
    let missing = if h.growth_records.is_empty() {
        "no growth record"
    } else {
        "no growth record that names a reviewer other than its author"
    };
    Some(Violation {
        suite: grew.join(", "),
        metric: Bound::UnrecordedGrowth,
        baseline: 0.0,
        limit: allowance as f64,
        current: h.growth as f64,
        fix: format!(
            "the change adds {} source line(s) net since {} ({}), more than growth_allowance ({allowance}), and \
             {missing}: {}",
            h.growth,
            h.from,
            h.target,
            crate::growth::JUSTIFY_GROWTH
        ),
    })
}

/// The new baseline: each bound at current where that is a gain (as `check`
/// calls it improved), else at the baseline's; a loosened bound at current, recorded with its reason. Refuses while
/// any other bound is violated. The picks a mutants run ran are recorded
/// by the caller (`landing::Record`), not in the baseline.
pub fn ratchet(
    base: &Baseline,
    m: &Measurement,
    cfg: &CheckConfig,
    loosen: &[Bound],
    reason: Option<&str>,
) -> Result<Baseline> {
    let verdict = check(base, m, cfg, None);
    let loosened = |b: Bound| loosen.contains(&b) && b.never_loosened().is_none();
    let mut faults: Vec<Fault> = verdict
        .violations
        .iter()
        .filter(|v| !loosened(v.metric))
        .map(|v| {
            let name = v.metric.name();
            let raise = if v.metric.never_loosened().is_some() {
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
    for &b in loosen {
        if let Some(fix) = b.never_loosened() {
            faults.push(Fault { fault: format!("--loosen {}: it is never loosened", b.name()), fix });
        }
    }
    // `test_seconds` is set only from a test run at a gate's priority.
    for s in m.suites.iter().filter(|s| s.untimed) {
        if !base.suites.contains_key(&s.suite) {
            faults.push(Fault {
                fault: format!(
                    "{}: its tests ran inside `--mutants`, at background priority: its test_seconds is no bound",
                    s.suite
                ),
                fix: format!("{TIME_THE_TESTS}, then `slimfit ratchet` again"),
            });
        }
    }
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
    let mut loosenings = Vec::new();
    let mut suites = BTreeMap::new();
    for s in &m.suites {
        let old = base.suites.get(&s.suite);
        let mut metrics = s.metrics.clone();
        for &bound in <Bound as clap::ValueEnum>::value_variants() {
            let (was, cur) = (bound.kept(old), bound.current(s, None));
            if loosened(bound)
                && let (Some(from), Some(to)) = (was, cur)
                && to > from
            {
                loosenings.push(Loosening {
                    suite: s.suite.clone(),
                    metric: bound,
                    from,
                    to,
                    reason: reason.unwrap_or_default().into(),
                });
            }
            let Some(was) = was.filter(|_| old.is_some()) else { continue };
            let gained = cur.filter(|c| loosened(bound) || bound.gain(cfg, was, *c));
            bound.keep(&mut metrics, gained.unwrap_or(was));
        }
        suites.insert(s.suite.clone(), metrics);
    }
    let mut next = Baseline::next(base, suites, m);
    next.loosened.extend(loosenings);
    Ok(next)
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::model::{MutantCounts, Mutated};

    fn suite(name: &str, metrics: Metrics, survived: Option<u64>) -> SuiteMeasurement {
        let change = |n| MutantCounts { killed: 3, survived: n, unviable: 0, equivalent: 0 };
        SuiteMeasurement {
            suite: name.into(),
            metrics,
            tests: vec![],
            uncovered: vec![],
            mutants: vec![],
            reach: None,
            mutated: survived.map(|n| Mutated { picks: vec![], change: change(n) }),
            command_seconds: 0.0,
            untimed: false,
        }
    }

    /// `survived`: the change's survivors, when mutants were measured.
    fn measurement(metrics: Metrics, survived: Option<u64>) -> Measurement {
        Measurement { tree: "t".into(), with_mutants: survived.is_some(), suites: vec![suite("s", metrics, survived)] }
    }

    /// A baseline, and a cached measurement, written before whole-codebase
    /// runs, the kill matrix, `mutants_at` and per-line records were removed still load: the
    /// removed fields are ignored, and the next ratchet writes the baseline
    /// without them.
    #[test]
    fn old_files_with_removed_fields_still_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".slimfit")).unwrap();
        let old = r#"{"suites":{"rust":{"prod_lines":1,"test_lines":1,"tests":1,"failures":0,"test_seconds":1.0,
            "covered_lines":1,"uncovered_lines":0,"mutants":{"killed":3},"mutant_files":{"a.rs":{}},"mutants_pending":["b.rs"]}},
            "mutants_at":{"commit":"c","dirty":["x"]}}"#;
        std::fs::write(dir.path().join(FILE), old).unwrap();
        let b = load(dir.path()).unwrap();
        assert_eq!(b.suites["rust"].tests, 1);
        assert!(!render(&b).unwrap().contains("mutants_at"));
        let cached = r#"{"tree":"t","with_mutants":true,"suites":[{"suite":"rust","metrics":{"prod_lines":1,"test_lines":1,
            "tests":1,"failures":0,"test_seconds":1.0,"covered_lines":1,"uncovered_lines":0},"tests":[],"uncovered":[],
            "mutants":[{"file":"a.rs","line":1,"description":"d","status":"killed","killed_by":["t"]}],"mutation_tests":["t"],
            "mutated":{"all":true,"files":[],"change":{"killed":0,"survived":0,"unviable":0}},"command_seconds":1.0,"load_slowdown":1.0}]}"#;
        let m: Measurement = serde_json::from_str(cached).unwrap();
        assert_eq!(m.suites[0].mutants.len(), 1);
    }

    /// A kept file holds up to its thresholds and not at them: 100 prod
    /// lines hold at 149, not 150; 4 commits since its 4 in 90 days
    /// hold at 3, not 4.
    #[test]
    fn a_kept_file_holds_below_its_thresholds_only() {
        let k = KeptFile {
            suite: "s".into(),
            file: "f.rs".into(),
            prod_lines: 100,
            commits_90d: 4,
            commits_total: 10,
            reason: "r".into(),
        };
        let holds = |lines, total| k.holds("s", "f.rs", lines, total);
        assert_eq!((holds(149, 13), holds(150, 13), holds(149, 14)), (true, false, false));
    }

    /// A kept span holds for the same lines of the same function, their text
    /// unchanged, in its own suite: grown past its lines, it does not.
    #[test]
    fn a_kept_span_holds_for_its_lines_as_kept_only() {
        let k = KeptSpan {
            suite: "s".into(),
            file: "a.rs".into(),
            start: 3,
            end: 6,
            function: Some("f".into()),
            hash: "h".into(),
            reason: "r".into(),
        };
        let sp = Span {
            file: "a.rs".into(),
            start: 3,
            end: 6,
            lines: 4,
            function: Some("f".into()),
            hash: Some("h".into()),
        };
        assert!(k.holds("s", &sp) && !k.holds("t", &sp));
        let grown = Span { end: 7, lines: 5, ..sp.clone() };
        let renamed = Span { function: Some("g".into()), ..sp.clone() };
        let unhashed = Span { hash: None, ..sp.clone() };
        assert_eq!([&grown, &renamed, &unhashed].map(|s| k.holds("s", s)), [false; 3]);
    }

    /// The next baseline carries the old one's verdicts over (a keep verdict
    /// only while its test's, or its span's, text is unchanged, or its suite
    /// was not seen)
    /// and takes the bounds it is given and the trends of the measurement.
    #[test]
    fn next_carries_the_verdicts_over_and_takes_the_measurements_trends() {
        use crate::model::{Reach, TestSource};
        let test = |name: &str, hash: &str| ReachTest {
            binary: "b".into(),
            name: name.into(),
            source: Some(TestSource { file: "t.rs".into(), line: 1, hash: hash.into(), shape: None }),
        };
        let kept = |suite: &str, name: &str| Kept {
            suite: suite.into(),
            test: format!("b::{name}"),
            hash: "h".into(),
            reason: "r".into(),
            ..Kept::default()
        };
        let span = |start, hash: &str| Span {
            file: "src/a.rs".into(),
            start,
            end: start + 1,
            lines: 2,
            function: Some("f".into()),
            hash: Some(hash.into()),
        };
        let kept_span = |suite: &str, start| KeptSpan {
            suite: suite.into(),
            file: "src/a.rs".into(),
            start,
            end: start + 1,
            function: Some("f".into()),
            hash: "h".into(),
            reason: "host only".into(),
        };
        let old = Baseline {
            suites: [("s".to_string(), m(10, 10.0)), ("gone".to_string(), m(1, 1.0))].into(),
            loosened: vec![Loosening {
                suite: "s".into(),
                metric: Bound::SurvivedMutants,
                from: 0.0,
                to: 1.0,
                reason: "equivalent".into(),
            }],
            kept: vec![kept("s", "same"), kept("s", "edited"), kept("s", "deleted"), kept("unseen", "x")],
            kept_files: vec![KeptFile {
                suite: "s".into(),
                file: "src/a.rs".into(),
                prod_lines: 10,
                commits_90d: 1,
                commits_total: 2,
                reason: "deep".into(),
            }],
            kept_spans: vec![kept_span("s", 1), kept_span("s", 5), kept_span("s", 9), kept_span("unseen", 1)],
            kept_mutants: vec![KeptMutant {
                suite: "s".into(),
                mutant: "a: m".into(),
                hash: "h".into(),
                reason: "r".into(),
            }],
            mutants_from: Some("abc".into()),
            measured: [("gone".to_string(), Measured::default())].into(),
        };
        let mut cur = measurement(m(8, 4.0), None);
        let tests = vec![test("same", "h"), test("edited", "h2")];
        cur.suites[0].reach = Some(Reach { tests, functions: BTreeMap::new(), lines: None });
        // The span at 1 as kept, the one at 5 edited, the one at 9 covered since.
        cur.suites[0].uncovered = vec![span(1, "h"), span(5, "h2")];
        let suites: BTreeMap<String, Metrics> = [("s".to_string(), m(9, 5.0))].into();
        let next = Baseline::next(&old, suites.clone(), &cur);
        let names: Vec<&str> = next.kept.iter().map(|k| k.test.as_str()).collect();
        assert_eq!(names, ["b::same", "b::x"]);
        assert_eq!(next.kept_spans, [kept_span("s", 1), kept_span("unseen", 1)]);
        assert_eq!(
            (&next.loosened, &next.kept_files, &next.kept_mutants, &next.mutants_from),
            (&old.loosened, &old.kept_files, &old.kept_mutants, &old.mutants_from)
        );
        assert_eq!(next.suites, suites);
        assert_eq!(next.measured, Measured::of(&cur));
        // Nothing to carry over from: none of the old verdicts.
        let fresh = Baseline::next(&Baseline::default(), suites.clone(), &cur);
        assert_eq!(fresh, Baseline { suites, measured: Measured::of(&cur), ..Baseline::default() });
    }

    fn base(metrics: Metrics) -> Baseline {
        Baseline { suites: [("s".to_string(), metrics)].into(), ..Baseline::default() }
    }

    fn m(uncovered: u64, secs: f64) -> Metrics {
        Metrics { uncovered_lines: uncovered, test_seconds: secs, ..Default::default() }
    }

    #[test]
    fn a_trend_that_falls_is_an_improvement_and_a_suite_new_to_the_baseline_still_fails() {
        let cfg = CheckConfig::default();
        let v = check(&base(m(10, 10.0)), &measurement(m(8, 9.0), Some(0)), &cfg, None);
        assert!(v.ok, "{v:?}");
        let improved = [Bound::TestSeconds, Bound::UncoveredLines];
        assert_eq!(v.improved.iter().map(|c| c.metric).collect::<Vec<_>>(), improved);
        // A suite new to the baseline is recorded, but its failures still fail.
        let v = check(&Baseline::default(), &measurement(Metrics { failures: 1, ..m(1, 1.0) }, None), &cfg, None);
        assert_eq!((v.new_suites, v.violations.len()), (vec!["s".to_string()], 1));
        assert_eq!(v.test_seconds, [Trend::of("s", None, 1.0)]);
        assert_eq!(v.uncovered_lines, [Trend::of("s", None, 1.0)]);
    }

    /// `uncovered_lines` over the baseline fails nothing (slimfit#67): the
    /// check is green and answers it, naming the uncovered spans that hold
    /// a line the change adds; one it falls within `coverage_slack` is not
    /// named, and the ratchet keeps the lower count, a drop counting only
    /// past the slack; `--loosen uncovered_lines` is refused with what a
    /// fix is.
    #[test]
    fn uncovered_lines_is_a_trend_that_names_the_changes_spans() {
        let cfg = CheckConfig::default();
        let span =
            |file: &str, start, end| Span { file: file.into(), start, end, lines: 2, function: None, hash: None };
        let mut more = measurement(m(14, 10.0), None);
        more.suites[0].uncovered = vec![span("a.rs", 3, 4), span("a.rs", 8, 9), span("b.rs", 1, 2)];
        let added: crate::landing::Lines =
            [("a.rs".to_string(), [(9, "x".to_string()), (20, "y".to_string())].into())].into();
        let held = Held { added, ..Held::default() };
        // Without a landing, no span is named.
        let v = check(&base(m(10, 10.0)), &more, &cfg, None);
        assert!(v.ok && v.uncovered_lines[0].new_spans.is_empty(), "{v:?}");
        let v = check(&base(m(10, 10.0)), &more, &cfg, Some(&held));
        assert!(v.ok && v.violations.is_empty() && v.improved.is_empty(), "{v:?}");
        assert_eq!(
            v.uncovered_lines,
            [Trend { new_spans: vec![span("a.rs", 8, 9)], ..Trend::of("s", Some(10.0), 14.0) }]
        );
        // Not risen: no span named, though the change adds one.
        let same = check(&base(m(14, 10.0)), &more, &cfg, Some(&held));
        assert!(same.uncovered_lines[0].new_spans.is_empty(), "{same:?}");
        let kept = |cur: u64| ratchet(&base(m(10, 10.0)), &measurement(m(cur, 10.0), None), &cfg, &[], None);
        assert_eq!([14, 9, 8].map(|c| kept(c).unwrap().suites["s"].uncovered_lines), [10, 10, 8]);
        // Beside the bound, what was measured: the rise shows there.
        assert_eq!(
            kept(14).unwrap().measured["s"],
            Measured { uncovered_lines: 14, test_seconds: Some(10.0), untimed: None }
        );
        let f = crate::fault::of(
            ratchet(&base(m(10, 10.0)), &more, &cfg, &[Bound::UncoveredLines], Some("x")).unwrap_err(),
        );
        assert!(
            f[0].fault.contains("never loosened") && f[0].fix.contains("a test that only runs them is no fix"),
            "{f:?}"
        );
    }

    /// `test_seconds` far over the baseline fails nothing: the check is
    /// green and answers it; the ratchet keeps the lower time, and refuses
    /// to loosen what bounds nothing (slimfit#59).
    #[test]
    fn test_seconds_is_a_trend_not_a_bound() {
        let cfg = CheckConfig::default();
        let slow = measurement(m(10, 100.0), None);
        let v = check(&base(m(10, 10.0)), &slow, &cfg, None);
        assert!(v.ok && v.violations.is_empty() && v.improved.is_empty(), "{v:?}");
        assert_eq!(v.test_seconds, [Trend::of("s", Some(10.0), 100.0)]);
        assert_eq!(ratchet(&base(m(10, 10.0)), &slow, &cfg, &[], None).unwrap().suites["s"].test_seconds, 10.0);
        let fast = measurement(m(10, 4.0), None);
        assert_eq!(ratchet(&base(m(10, 10.0)), &fast, &cfg, &[], None).unwrap().suites["s"].test_seconds, 4.0);
        let f =
            crate::fault::of(ratchet(&base(m(10, 10.0)), &slow, &cfg, &[Bound::TestSeconds], Some("x")).unwrap_err());
        assert!(f[0].fault.contains("never loosened") && f[0].fix.contains("--kind slow-test"), "{f:?}");
    }

    /// A landing that adds source lines or removes tests with no record
    /// fails the check as `unrecorded_mutants`, its fix naming the first
    /// line (or test) and what is
    /// missing; once held (no suite unrecorded), or with no landing, it
    /// passes, and it never counts as improved.
    #[test]
    fn a_landing_without_a_record_fails_as_unrecorded_mutants() {
        let cfg = CheckConfig::default();
        let ten = measurement(m(10, 10.0), None);
        let unrecorded = [("s".to_string(), crate::landing::Unrecorded { lines: 3, tests: 0, first: "a.rs:7".into() })];
        let held = |records: Vec<String>| Held {
            target: "origin/HEAD".into(),
            from: "abc".into(),
            lines: 3,
            records,
            unrecorded: unrecorded.clone().into(),
            ..Held::default()
        };
        let v = check(&base(m(10, 10.0)), &ten, &cfg, Some(&held(vec![])));
        assert!(!v.ok, "{v:?}");
        let u = &v.violations[0];
        assert_eq!(
            (v.violations.len(), u.metric, u.baseline, u.limit, u.current),
            (1, Bound::UnrecordedMutants, 0.0, 0.0, 3.0)
        );
        assert!(
            u.fix.starts_with(
                "the change adds 3 source line(s) since abc (origin/HEAD), the first at a.rs:7, and no record of its \
                 mutants: list the change's mutants"
            ),
            "{u:?}"
        );
        let v = check(&base(m(10, 10.0)), &ten, &cfg, Some(&held(vec!["r.json".into()])));
        assert!(
            v.violations[0].fix.contains("and no record of its mutants that names a picker other than its author: ")
        );
        // A removed test counts as a line does; with no line, the first test is named.
        let removed = crate::landing::Unrecorded { lines: 0, tests: 2, first: "t.rs:4: gone".into() };
        let only_tests = Held { unrecorded: [("s".to_string(), removed)].into(), ..held(vec![]) };
        let u = check(&base(m(10, 10.0)), &ten, &cfg, Some(&only_tests)).violations.remove(0);
        assert_eq!(u.current, 2.0);
        assert!(
            u.fix.starts_with("the change removes 2 test(s) since abc (origin/HEAD), the first at t.rs:4: gone,"),
            "{u:?}"
        );
        let recorded = Held { unrecorded: Default::default(), ..held(vec![]) };
        for h in [Some(&recorded), None] {
            let v = check(&base(m(10, 10.0)), &ten, &cfg, h);
            assert!(v.ok && v.violations.is_empty() && v.improved.is_empty(), "{v:?}");
        }
    }

    /// No baseline keeps a mutant count: a survivor in the change fails the
    /// check, in a new suite too, until it is killed or loosened with a
    /// reason; `unrecorded_mutants` is never loosened.
    #[test]
    fn the_changes_own_survivors_fail_until_loosened() {
        let cfg = CheckConfig::default();
        let two = measurement(m(10, 10.0), Some(2));
        for b in [base(m(10, 10.0)), Baseline::default()] {
            let v = check(&b, &two, &cfg, None);
            assert_eq!(
                v.violations.iter().map(|v| (v.metric, v.limit, v.current)).collect::<Vec<_>>(),
                [(Bound::SurvivedMutants, 0.0, 2.0)]
            );
            assert!(ratchet(&b, &two, &cfg, &[], None).is_err());
        }
        let next = ratchet(&base(m(10, 10.0)), &two, &cfg, &[Bound::SurvivedMutants], Some("equivalent")).unwrap();
        let l = &next.loosened[0];
        assert_eq!((l.metric, l.from, l.to, l.reason.as_str()), (Bound::SurvivedMutants, 0.0, 2.0, "equivalent"));
        let clean = ratchet(&base(m(10, 10.0)), &measurement(m(10, 10.0), Some(0)), &cfg, &[], None).unwrap();
        assert!(clean.loosened.is_empty());
        let never = ratchet(&base(m(10, 10.0)), &two, &cfg, &[Bound::UnrecordedMutants], Some("x")).unwrap_err();
        let f = crate::fault::of(never);
        assert!(f.iter().any(|f| f.fault.contains("never loosened") && f.fix.contains("--background")), "{f:?}");
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

    /// A test run inside `--mutants` holds no `test_seconds`: not checked,
    /// not ratcheted, not loosened, and no new suite's bound is set from it.
    #[test]
    fn a_test_run_inside_mutants_neither_checks_nor_moves_test_seconds() {
        let cfg = CheckConfig::default();
        let mut slow = measurement(m(10, 31.1), Some(0));
        slow.suites[0].untimed = true;
        let v = check(&base(m(10, 10.0)), &slow, &cfg, None);
        assert!(v.ok && v.improved.is_empty(), "{v:?}");
        assert_eq!((v.time_unchecked, v.test_seconds), (vec!["s".to_string()], vec![]));
        let mut fast = slow.clone();
        fast.suites[0].metrics.test_seconds = 1.0;
        for cur in [&slow, &fast] {
            assert_eq!(ratchet(&base(m(10, 10.0)), cur, &cfg, &[], None).unwrap().suites["s"].test_seconds, 10.0);
        }
        // What was measured says why it holds no time: not measured, not dropped unsaid.
        let measured = &ratchet(&base(m(10, 10.0)), &slow, &cfg, &[], None).unwrap().measured["s"];
        assert_eq!((measured.test_seconds, measured.untimed.as_deref()), (None, Some(UNTIMED)));
        let refused = |b: &Baseline, loosen: &[Bound]| {
            let f = crate::fault::of(ratchet(b, &slow, &cfg, loosen, None).unwrap_err());
            assert!(
                f[0].fault.contains("ran inside `--mutants`") && f[0].fix.contains("plain `slimfit check`"),
                "{f:?}"
            );
        };
        refused(&Baseline::default(), &[]);
        assert!(check(&base(m(10, 10.0)), &measurement(m(10, 31.1), None), &cfg, None).time_unchecked.is_empty());
    }

    /// Failing tests fail the check whatever the baseline records; once
    /// they pass that is a gain, which `ratchet` locks in; never loosened.
    #[test]
    fn failures_are_held_to_none_and_their_fix_is_a_gain() {
        let cfg = CheckConfig::default();
        let failing = base(Metrics { failures: 2, ..m(10, 10.0) });
        let v = check(&failing, &measurement(Metrics { failures: 1, ..m(10, 10.0) }, None), &cfg, None);
        assert_eq!(v.violations.iter().map(|v| (v.metric, v.limit)).collect::<Vec<_>>(), [(Bound::Failures, 0.0)]);
        let fixed = measurement(m(10, 10.0), None);
        let v = check(&failing, &fixed, &cfg, None);
        assert_eq!(v.improved, [Change { suite: "s".into(), metric: Bound::Failures, baseline: 2.0, current: 0.0 }]);
        assert_eq!(ratchet(&failing, &fixed, &cfg, &[], None).unwrap().suites["s"].failures, 0);
        let f = crate::fault::of(ratchet(&failing, &fixed, &cfg, &[Bound::Failures], Some("x")).unwrap_err());
        assert!(f[0].fault.contains("never loosened") && f[0].fix.starts_with("make the failing tests pass"), "{f:?}");
    }

    #[test]
    fn ratchet_keeps_the_better_time() {
        let cfg = CheckConfig::default();
        let next = ratchet(&base(m(10, 10.0)), &measurement(m(8, 11.0), None), &cfg, &[], None).unwrap();
        let s = &next.suites["s"];
        assert_eq!((s.uncovered_lines, s.test_seconds), (8, 10.0));
    }

    #[test]
    fn loosening_needs_a_reason_and_is_recorded() {
        let cfg = CheckConfig::default();
        let (b, cur) = (base(m(10, 10.0)), measurement(m(10, 10.0), Some(2)));
        let fixes = |r: Result<Baseline>| -> Vec<String> {
            crate::fault::of(r.unwrap_err()).into_iter().map(|f| f.fix).collect()
        };
        let f = fixes(ratchet(&b, &cur, &cfg, &[], None));
        assert!(
            f[0].ends_with("or, if the rise is right, `slimfit ratchet --loosen survived_mutants --reason \"<why>\"`")
        );
        assert!(fixes(ratchet(&b, &cur, &cfg, &[Bound::SurvivedMutants], None))[0].starts_with("add `--reason"));
        // Every refusal at once: each violated bound, and the missing reason.
        let worse = measurement(Metrics { failures: 1, ..m(12, 20.0) }, Some(2));
        let f = fixes(ratchet(&b, &worse, &cfg, &[Bound::UncoveredLines], None));
        assert_eq!(f.len(), 4, "{f:?}");
        assert!(f[0].starts_with("make these tests pass") && !f[0].contains("--loosen failures"), "{f:?}");
        assert!(f[1].contains("--loosen survived_mutants") && f[2].starts_with("uncovered_lines is a trend"), "{f:?}");
        assert!(f[3].starts_with("add `--reason"), "{f:?}");
        let next = ratchet(&b, &cur, &cfg, &[Bound::SurvivedMutants], Some("equivalent")).unwrap();
        assert_eq!((next.loosened[0].to, next.loosened[0].reason.as_str()), (2.0, "equivalent"));
        // A loosened bound that did not rise is not recorded as raised.
        let same = ratchet(&b, &measurement(m(10, 10.0), Some(0)), &cfg, &[Bound::SurvivedMutants], Some("x"));
        assert!(same.unwrap().loosened.is_empty());
    }
}
