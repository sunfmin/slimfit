//! Turning a measurement into work: one task at a time, cheapest and
//! safest kinds first, each with its evidence and the command that accepts it.

use crate::config::Config;
use crate::git;
use crate::loc::{Classifier, Kind};
use crate::model::{Measurement, MutantStatus, ReachTest, SuiteMeasurement};
use crate::ratchet::{Baseline, Kept, KeptFile};
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, agent_help::Help)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    /// A test fails. Fix it first; nothing else is measured honestly.
    #[value(verbatim_doc_comment)]
    FailingTest,
    /// The change adds source lines and no record of its mutants: the gate's `check` fails until it does (see [what the gate holds](help:mutants-scope)). Read your change, pick at most {const:PICKS_MAX} of the listed mutants on the decisions it makes, run them, and ratchet; or record why none is worth one.
    #[value(verbatim_doc_comment)]
    PickMutants,
    /// Lines no test runs. No caller: delete them. A user path needs them: test it through its module's interface, with a fast test. A feature nobody uses: don't delete it, escalate: that is a product decision.
    #[value(verbatim_doc_comment)]
    Uncovered,
    /// Coverage proposes a test whose every source line another test runs too, or whose lines mostly overlap another's. Overlap leaves out the lines more than {const:COMMON_LINES} of the suite's tests run (their shared setup), so two tests overlap by what they run past it. Read both tests' assertions and decide: delete it, merge the two, or keep it (`slimfit keep`). Only where the test run records coverage per test ({const:PER_TEST}); runs no mutant.
    #[value(verbatim_doc_comment)]
    RedundantTest,
    /// One test holds a large share of the suite's time. Make it cheaper without testing less, or merge it into faster ones.
    #[value(verbatim_doc_comment)]
    SlowTest,
    /// A picked mutant of the change no test notices. Assert the behaviour through the interface with a fast test, or simplify the code away. Comes from a `slimfit check --mutants <name>...` of this tree.
    #[value(verbatim_doc_comment)]
    SurvivingMutant,
    /// A big file that keeps changing: a proposal from size and churn, not a verdict. Load the codebase-design and improve-codebase-architecture skills and decide: design the deeper interface first, then move to it one passing step at a time; or keep it as it is (`slimfit keep <file>`).
    #[value(verbatim_doc_comment)]
    Deepen,
}

impl TaskKind {
    /// The command that must pass after a task of this kind is done.
    pub fn accept(self) -> &'static str {
        // Every kind's: the change it makes then picks its own mutants, as
        // any change that adds source lines does (a pick-mutants task).
        "slimfit check"
    }

    /// Every accept command, each once, in the order of the kinds.
    pub fn accepts() -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for k in <TaskKind as clap::ValueEnum>::value_variants() {
            if !out.contains(&k.accept()) {
                out.push(k.accept());
            }
        }
        out
    }
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

#[cfg(test)]
impl Task {
    /// A task of no substance.
    pub fn example() -> Self {
        let kind = TaskKind::FailingTest;
        Task {
            kind,
            suite: String::new(),
            summary: String::new(),
            files: vec![],
            evidence: Value::Null,
            steps: String::new(),
            accept: kind.accept().into(),
        }
    }
}

/// How many tasks of one kind in one suite `next --all` lists; the rest are
/// only counted.
pub const LISTED_PER_KIND: usize = 5;
/// As [`LISTED_PER_KIND`], for deepen tasks.
pub const LISTED_DEEPEN: usize = 3;
/// How far back in git history `deepen` counts a file's changes, in days.
pub const DEEPEN_DAYS: u32 = 90;

fn per_kind(kind: TaskKind) -> usize {
    if kind == TaskKind::Deepen { LISTED_DEEPEN } else { LISTED_PER_KIND }
}

/// The tasks worth listing: the first few of each kind in each suite, in
/// order. Count before capping: what is left is every task, not the list.
pub fn cap(tasks: Vec<Task>) -> Vec<Task> {
    let mut seen: BTreeMap<(String, TaskKind), usize> = BTreeMap::new();
    tasks
        .into_iter()
        .filter(|t| {
            let n = seen.entry((t.suite.clone(), t.kind)).or_default();
            *n += 1;
            *n <= per_kind(t.kind)
        })
        .collect()
}

pub fn all(repo: &Path, cfg: &Config, m: &Measurement, base: &Baseline) -> Result<Vec<Task>> {
    let mut out = pick_mutants(repo, cfg, m.with_mutants)?;
    for (sc, s) in cfg.suites.iter().zip(&m.suites) {
        out.extend(of_measurement(s, &base.kept));
        out.extend(deepen(repo, &Classifier::new(&sc.resolve())?, &s.suite, &base.kept_files)?);
    }
    out.sort_by_key(|t| t.kind);
    Ok(out)
}

/// The tasks a suite's measurement alone shows (everything but `deepen`,
/// which reads git history), less the redundant tests `kept`.
pub fn of_measurement(s: &SuiteMeasurement, kept: &[Kept]) -> Vec<Task> {
    [failing(s), uncovered(s), redundant(s, kept), slow(s), surviving(s)].concat()
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
            accept: TaskKind::FailingTest.accept().into(),
        })
        .collect()
}

/// A task per suite whose change adds source lines and no record of its
/// mutants (none due once `picked` ran: `ratchet` records them), with what
/// its tool lists of the change to pick from.
fn pick_mutants(repo: &Path, cfg: &Config, picked: bool) -> Result<Vec<Task>> {
    let (_, unrecorded) = crate::landing::hold(repo, cfg, picked)?;
    if unrecorded.is_empty() {
        return Ok(vec![]);
    }
    let mut candidates = crate::measure::candidates(repo, cfg)?;
    Ok(unrecorded
        .into_iter()
        .map(|v| {
            let listed = candidates.remove(&v.suite).unwrap_or_default();
            let files: BTreeSet<String> = listed.iter().map(|c| c.file.clone()).collect();
            Task {
                kind: TaskKind::PickMutants,
                summary: format!(
                    "the change adds {} source line(s) and no record of its mutants: pick the few worth running, of {}",
                    v.current,
                    listed.len()
                ),
                suite: v.suite,
                files: files.into_iter().collect(),
                evidence: json!({ "lines": v.current, "candidates": listed }),
                steps: format!(
                    "Read your change. Of `candidates` (what `slimfit check --mutants` lists), pick at most {} on the \
                     decisions it makes: comparisons, boolean logic, match guards, a function whose return value \
                     matters. Skip those that cannot build, such as a body replaced by `Default::default()` for a \
                     type with no default. Run them at background priority: `factorium worker exclusive --background \
                     -- slimfit check --mutants <name>... --why <why>...` (outside factorium, `nice -n 19 slimfit \
                     check ...`), one one-line `--why` per pick. A survivor: assert what it changes with a fast test, \
                     or, if it changes nothing observable, `slimfit ratchet --loosen survived_mutants --reason \
                     \"<why>\"`. Then `slimfit ratchet` writes the record. If nothing in the change is worth a mutant, \
                     `slimfit ratchet --no-mutants \"<why>\"` records that instead. Commit .slimfit/ with the change.",
                    crate::measure::PICKS_MAX
                ),
                accept: TaskKind::PickMutants.accept().into(),
            }
        })
        .collect())
}

fn uncovered(s: &SuiteMeasurement) -> Vec<Task> {
    s.uncovered
        .iter()
        .filter(|sp| sp.lines >= 2)
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
                    The new test must be fast (milliseconds, no sleeps, network or process spawns): if only a slow test can reach the line, delete the code or escalate, never add the slow test. \
                    It is a product feature nobody seems to use: do not delete it; removing features is a product decision, so escalate with this evidence."
                .into(),
            accept: TaskKind::Uncovered.accept().into(),
        })
        .collect()
}

/// Two tests overlap enough to be merge candidates when the lines both run
/// are at least this share of the lines either runs (ADR 0006).
pub const MERGE_OVERLAP: f64 = 0.8;

/// A line more than this share of a suite's tests run is shared setup (a
/// scenario's world, a daemon's tick): overlap is computed without it (ADR
/// 0006).
pub const COMMON_LINES: f64 = 0.5;

/// Redundant-test candidates from per-test coverage (ADR 0006): a test whose
/// lines are all run by another test too (when both run the same lines, the
/// slower of the two is proposed), or that shares at least [`MERGE_OVERLAP`]
/// of the lines either runs with another (the slower of the two is
/// proposed). Overlap is computed without the lines more than
/// [`COMMON_LINES`] of the tests run, which shared setup makes, not what a
/// test checks; a subset is of all lines, as setup cannot make one. Each is named once, with the other test it most overlaps;
/// the slowest first. Coverage only proposes:
/// the steps decide by reading the assertions. A test kept, unchanged since,
/// is not proposed.
fn redundant(s: &SuiteMeasurement, kept: &[Kept]) -> Vec<Task> {
    let Some((reach, per_test)) = s.reach.as_ref().and_then(|r| Some((r, r.lines.as_ref()?))) else { return vec![] };
    let seconds = |id: &str| s.tests.iter().find(|t| t.id == id).map_or(0.0, |t| t.seconds);
    let ids: Vec<String> = reach.tests.iter().map(ReachTest::id).collect();
    // Each test, the (file, line)s it runs, and its seconds.
    let lines: Vec<_> = ids
        .iter()
        .zip(per_test)
        .map(|(t, files)| {
            let ls: BTreeSet<(&str, u32)> =
                files.iter().flat_map(|(f, ls)| ls.iter().map(move |l| (f.as_str(), *l))).collect();
            (t, ls, seconds(t))
        })
        .filter(|(_, ls, _)| !ls.is_empty())
        .collect();
    // Each test's own lines: those COMMON_LINES of the tests or fewer run.
    let mut runs: HashMap<(&str, u32), usize> = HashMap::new();
    for l in lines.iter().flat_map(|(_, ls, _)| ls) {
        *runs.entry(*l).or_default() += 1;
    }
    let shared = COMMON_LINES * lines.len() as f64;
    let own: Vec<BTreeSet<_>> =
        lines.iter().map(|(_, ls, _)| ls.iter().filter(|l| runs[*l] as f64 <= shared).copied().collect()).collect();
    let at = |id: &str| reach.tests.iter().find(|t| t.id() == id);
    let mut out: Vec<(f64, Task)> = Vec::new();
    for ((t, mine, secs), mine_own) in lines.iter().zip(&own) {
        if at(t).is_some_and(|rt| kept.iter().any(|k| k.holds(&s.suite, rt))) {
            continue;
        }
        // The other test with the most overlap; its share and whether it runs all of mine.
        let best = lines
            .iter()
            .zip(&own)
            .filter(|((o, _, _), _)| o != t)
            .map(|((o, theirs, o_secs), theirs_own)| {
                let slower = (secs, t) > (o_secs, o);
                let within = mine.is_subset(theirs) && (mine.len() < theirs.len() || slower);
                // Own lines that only overlap: the pair is named once, by its slower test.
                let both = mine_own.intersection(theirs_own).count();
                let overlap = both < mine_own.len() && both < theirs_own.len() && slower;
                let union = mine_own.union(theirs_own).count();
                (o, if union == 0 { 0.0 } else { both as f64 / union as f64 }, within, overlap)
            })
            .filter(|(_, share, within, overlap)| *within || (*overlap && *share >= MERGE_OVERLAP))
            .map(|(o, share, within, _)| (o, share, within))
            .max_by(|a, b| (a.2, a.1).partial_cmp(&(b.2, b.1)).unwrap_or(std::cmp::Ordering::Equal).then(b.0.cmp(a.0)));
        let Some((other, share, within)) = best else { continue };
        out.push((
            *secs,
            Task {
                kind: TaskKind::RedundantTest,
                suite: s.suite.clone(),
                summary: if within {
                    format!("{other} runs every line {t} runs ({} lines)", mine.len())
                } else {
                    format!("{t} shares {:.0}% of its lines past the suite's shared setup with {other}", share * 100.0)
                },
                files: [at(t), at(other)].into_iter().flatten().filter_map(|rt| Some(rt.source.as_ref()?.file.clone())).collect::<BTreeSet<_>>().into_iter().collect(),
                evidence: json!({ "test": t, "seconds": secs, "lines": mine.len(), "other_test": other }),
                steps: "Coverage only proposes; the assertions decide. Read both tests. For every assertion of `test`, find the \
                        assertion in `other_test` (or another test you name) that checks the same outcome on the same setup. Then do one: \
                        DELETE `test` when every assertion maps (put the mapping in the commit message). \
                        MERGE when some assertion does not map but the two build compatible setups: fold the unmapped assertions, and the setup they need, \
                        into one test, or turn both into one test over a table of cases, so the setup is paid once; the merged test keeps every \
                        assertion of both, each failing with its own message, and the commit message lists where each assertion went. \
                        KEEP both when the setups conflict (folding one changes what the other checks) or a merge would hide which behaviour failed: \
                        record it with `slimfit keep <test> --reason \"<why>\"` and commit `.slimfit/baseline.json`, so `test` is not proposed again \
                        until its text changes. \
                        No mutant is run for this: the accept command holds that no line becomes uncovered, no test fails and test time does not \
                        rise; after a merge, `test_seconds` must fall."
                    .into(),
                accept: TaskKind::RedundantTest.accept().into(),
            },
        ));
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().map(|(_, t)| t).collect()
}

fn slow(s: &SuiteMeasurement) -> Vec<Task> {
    let total = s.metrics.test_seconds;
    let mut tests: Vec<_> = s.tests.iter().filter(|t| t.seconds >= 0.5 && t.seconds >= total * 0.05).collect();
    tests.sort_by(|a, b| b.seconds.total_cmp(&a.seconds));
    tests
        .into_iter()
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
            accept: TaskKind::SlowTest.accept().into(),
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
        .map(|(file, ms)| Task {
            kind: TaskKind::SurvivingMutant,
            suite: s.suite.clone(),
            summary: format!("{} mutants survive in {file}", ms.len()),
            files: vec![file.to_string()],
            evidence: json!({ "survivors": ms }),
            steps: "For each survivor: the change it makes is behaviour no test checks. If the behaviour matters, add an assertion through the module's interface that the mutant breaks. \
                    If it does not matter, the code is more than it needs to be: simplify it until the mutant cannot exist. \
                    The new assertion must be fast: add it to a fast test, never a slow one; if only a slow test could catch the mutant, simplify the code instead."
                .into(),
            accept: TaskKind::SurvivingMutant.accept().into(),
        })
        .collect()
}

/// The suite that counts `path` (relative to the repo) as an existing
/// source file, with its classifier: where a deepen task's file belongs.
pub fn source_suite(repo: &Path, cfg: &Config, path: &str) -> Result<Option<(String, Classifier)>> {
    if !repo.join(path).is_file() {
        return Ok(None);
    }
    for sc in &cfg.suites {
        let c = Classifier::new(&sc.resolve())?;
        if c.kind(path) == Some(Kind::Source) {
            return Ok(Some((sc.name.clone(), c)));
        }
    }
    Ok(None)
}

/// How many commits changed `file`: in the last [`DEEPEN_DAYS`], and ever.
pub fn churn(repo: &Path, file: &str) -> (u64, u64) {
    let count = |args: &[&str]| {
        let out = git::run(repo, &[&["log", "--no-merges", "--format=%H"], args, &["--", file]].concat());
        out.unwrap_or_default().lines().count() as u64
    };
    (count(&[&format!("--since={DEEPEN_DAYS}.days")]), count(&[]))
}

/// Source files that keep changing and are big: where a deeper module pays
/// off soonest. A file kept as it is (`kept`) is not proposed until it has
/// grown or churned past the verdict's thresholds.
fn deepen(repo: &Path, classifier: &Classifier, suite: &str, kept: &[KeptFile]) -> Result<Vec<Task>> {
    let log =
        git::run(repo, &["log", &format!("--since={DEEPEN_DAYS}.days"), "--no-merges", "--format=", "--name-only"])
            .unwrap_or_default();
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
        // Only a file with a verdict pays for counting its whole history.
        let held = || kept.iter().filter(|k| k.file == f).any(|k| k.holds(suite, f, lines, churn(repo, f).1));
        if lines >= 100 && !held() {
            hot.push((f.to_string(), n, lines));
        }
    }
    hot.sort_by_key(|(f, n, l)| (std::cmp::Reverse(n * l), f.clone()));
    Ok(hot
        .into_iter()
        .map(|(f, n, l)| Task {
            kind: TaskKind::Deepen,
            suite: suite.to_string(),
            summary: format!("{f}: {l} lines, changed in {n} commits in {DEEPEN_DAYS} days"),
            files: vec![f.clone()],
            evidence: json!({ "file": f, "prod_lines": l, "commits_90d": n }),
            steps: "Size and churn only propose; the module decides. Load the codebase-design and improve-codebase-architecture skills. Look at the module this file belongs to: \
                    shallow interfaces, pass-through functions, one concept spread over many small modules, tests that reach past the interface. Then do one: \
                    CHANGE it: write the deeper interface down first (GLOSSARY names, ADR if it settles a trade-off), then move to it one step per commit, each step passing the accept command. \
                    KEEP it when it is already deep enough and nothing is worth changing: record why with `slimfit keep <file> --reason \"<why>\"` and commit \
                    `.slimfit/baseline.json`; `slimfit keep --help` says when it is proposed again."
                .into(),
            accept: TaskKind::Deepen.accept().into(),
        })
        .collect())
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::model::{Metrics, TestCase};

    #[test]
    fn cap_keeps_the_first_few_of_each_kind_in_each_suite() {
        let task = |suite: &str, kind| Task { kind, suite: suite.into(), ..Task::example() };
        let mut all = Vec::new();
        for suite in ["a", "b"] {
            all.extend((0..7).map(|_| task(suite, TaskKind::Uncovered)));
            all.extend((0..4).map(|_| task(suite, TaskKind::Deepen)));
        }
        let kept = cap(all);
        let n = |suite: &str, kind| kept.iter().filter(|t| t.suite == suite && t.kind == kind).count();
        assert_eq!((n("a", TaskKind::Uncovered), n("a", TaskKind::Deepen)), (5, 3));
        assert_eq!((n("b", TaskKind::Uncovered), n("b", TaskKind::Deepen)), (5, 3));
    }

    fn suite(tests: impl IntoIterator<Item = (&'static str, f64, Vec<u32>)>) -> SuiteMeasurement {
        let mut reach = crate::model::Reach { lines: Some(vec![]), ..Default::default() };
        let mut cases = Vec::new();
        for (name, secs, lines) in tests {
            let source = crate::model::TestSource { file: format!("tests/{name}.rs"), line: 1, hash: "h".into() };
            reach.tests.push(ReachTest { binary: "t".into(), name: name.into(), source: Some(source) });
            reach.lines.as_mut().unwrap().push([("a.rs".to_string(), lines.into_iter().collect())].into());
            cases.push(TestCase { id: format!("t::{name}"), seconds: secs, passed: true, start: None });
        }
        SuiteMeasurement {
            suite: "s".into(),
            metrics: Metrics::default(),
            tests: cases,
            uncovered: vec![],
            mutants: vec![],
            reach: Some(reach),
            mutated: None,
            command_seconds: 0.0,
            load_slowdown: 1.0,
            host_load: 0.0,
            untimed: false,
        }
    }

    /// A big file that keeps changing is proposed; kept, it is not, until
    /// its prod lines grow by KEPT_GROWTH or as many commits change it again
    /// as made it a candidate; a verdict on another suite does not hold.
    #[test]
    fn a_kept_file_returns_once_it_grows_or_churns_past_the_verdict() {
        use crate::measure::t::git;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        let c = Classifier::new(&crate::config::preset(crate::config::Preset::Rust)).unwrap();
        let commit = |lines: usize, note: &str| {
            let body: String = (0..lines).map(|i| format!("pub const C{i}: u8 = {};\n", note.len())).collect();
            std::fs::create_dir_all(repo.join("src")).unwrap();
            std::fs::write(repo.join("src/big.rs"), format!("// {note}\n{body}")).unwrap();
            git(repo, &["add", "."]);
            git(repo, &["commit", "-qm", &format!("c {note}")]);
        };
        for i in 0..5 {
            commit(120, &"x".repeat(i));
        }
        let proposed = |kept: &[KeptFile]| deepen(repo, &c, "s", kept).unwrap().len();
        assert_eq!(proposed(&[]), 1);
        let (commits_90d, commits_total) = churn(repo, "src/big.rs");
        assert_eq!((commits_90d, commits_total), (5, 5));
        let prod_lines = c.lines(Kind::Source, &std::fs::read_to_string(repo.join("src/big.rs")).unwrap()).prod;
        let verdict = |suite: &str| KeptFile {
            suite: suite.into(),
            file: "src/big.rs".into(),
            prod_lines,
            commits_90d,
            commits_total,
            reason: "deep enough".into(),
        };
        assert_eq!(proposed(&[verdict("s")]), 0);
        assert_eq!(proposed(&[verdict("other suite")]), 1);
        // Growth just short of the threshold, in one commit: still kept.
        let short = (prod_lines as f64 * (1.0 + crate::ratchet::KEPT_GROWTH)) as usize - 2;
        commit(short, "grown");
        assert_eq!(proposed(&[verdict("s")]), 0);
        // Past it: proposed again.
        commit(short + 2, "grown more");
        assert_eq!(proposed(&[verdict("s")]), 1);
        // Back to its size: the 5 commits since reach the 5 that made it a candidate.
        let back = (0..3).map(|i| "y".repeat(10 + i));
        for note in back {
            commit(120, &note);
        }
        let mut fewer = verdict("s");
        fewer.commits_total += 1;
        assert_eq!(proposed(&[fewer]), 0);
        assert_eq!(proposed(&[verdict("s")]), 1);
    }

    /// A failing test is a task, with the log to read; a passing one is not.
    #[test]
    fn a_failing_test_is_a_task_with_its_log() {
        let mut s = suite([("ok", 0.1, vec![1]), ("broken", 0.1, vec![2])]);
        s.tests[1].passed = false;
        let tasks = failing(&s);
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].kind, tasks[0].summary.as_str()), (TaskKind::FailingTest, "t::broken fails"));
        assert_eq!(tasks[0].evidence["log"], ".slimfit/out/s/test.log");
    }

    /// Each proposed test with the other test it names.
    fn pairs(s: &SuiteMeasurement, kept: &[Kept]) -> Vec<(String, String)> {
        redundant(s, kept)
            .into_iter()
            .map(|t| {
                (
                    t.evidence["test"].as_str().unwrap().to_string(),
                    t.evidence["other_test"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    /// A test whose lines another runs too is proposed (of two that run the
    /// same lines, the slower); a test overlapping another by at least
    /// MERGE_OVERLAP is proposed for a merge; one apart from the rest, or
    /// overlapping less, is not; without per-test coverage, none is.
    #[test]
    fn redundant_tests_are_proposed_from_per_test_lines() {
        let tests = [("wide", 0.1, 1..=10), ("inside", 0.3, 2..=4), ("twin", 0.2, 1..=10), ("near", 0.1, 20..=29)];
        let tests =
            tests.into_iter().chain([("near2", 0.5, 21..=30), ("apart", 0.1, 40..=45), ("loose", 0.1, 41..=50)]);
        let s = suite(tests.map(|(n, secs, ls)| (n, secs, ls.collect())));
        let got = |kept: &[Kept]| pairs(&s, kept);
        let want = [("near2", "near"), ("inside", "twin"), ("twin", "wide")];
        assert_eq!(got(&[]), want.map(|(a, b)| (format!("t::{a}"), format!("t::{b}"))));
        assert_eq!(redundant(&s, &[])[0].files, ["tests/near.rs", "tests/near2.rs"]);
        // Kept, it is not proposed again until its text changes.
        let keep = |suite: &str, hash: &str| Kept {
            suite: suite.into(),
            test: "t::near2".into(),
            hash: hash.into(),
            reason: "r".into(),
        };
        assert_eq!(got(&[keep("s", "h")]).len(), 2);
        assert_eq!(got(&[keep("s", "changed")]).len(), 3);
        assert_eq!(got(&[keep("other suite", "h")]).len(), 3);
        let functions_only = crate::model::Reach { lines: None, ..s.reach.clone().unwrap() };
        assert!(redundant(&SuiteMeasurement { reach: Some(functions_only), ..s }, &[]).is_empty());
    }

    /// The lines more than COMMON_LINES of the tests run (their shared
    /// setup) make no overlap: tests that share only them are not proposed;
    /// two that run the same lines past the setup are.
    #[test]
    fn lines_most_tests_run_are_left_out_of_overlap() {
        let with_setup = |own: std::ops::RangeInclusive<u32>| (100..=190).chain(own).collect::<Vec<_>>();
        let tests = [
            ("a", 0.1, with_setup(1..=5)),
            ("b", 0.1, with_setup(11..=15)),
            ("c", 0.1, with_setup(21..=25)),
            ("same", 0.2, with_setup(31..=35)),
            ("same2", 0.1, with_setup(31..=35)),
        ];
        let s = suite(tests);
        assert_eq!(pairs(&s, &[]), [("t::same".to_string(), "t::same2".to_string())]);
        assert_eq!(redundant(&s, &[])[0].evidence["lines"], 96);
    }
}
